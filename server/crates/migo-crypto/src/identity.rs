//! Long-term identities and prekeys.
//!
//! A Migo device has two long-term key pairs, not one:
//!
//! * **Ed25519** for signatures — proving that a prekey really came from this
//!   device, and signing mesh frames between servers.
//! * **X25519** for Diffie-Hellman — the identity half of X3DH.
//!
//! Signal uses a single Curve25519 key for both via XEdDSA, converting between
//! the Edwards and Montgomery forms. That saves 32 bytes per published identity
//! and costs a birational map that has to be implemented correctly in three
//! languages. Two separate keys is the boring choice, and boring is the right
//! default in cryptographic code: nothing here is a novel construction, so there
//! is nothing here to get subtly wrong.
//!
//! The wire form of a published identity is `signing || exchange`, 64 bytes, in
//! that order. Both halves are needed to talk to a device, so splitting them
//! across two fields would only create a state where one is present without the
//! other.
//!
//! # What the server holds
//!
//! Public halves only. [`IdentitySecret`] never leaves the device that generated
//! it and has no serialisation to anything but the device's own encrypted
//! storage. There is no server-side key escrow, no "recover my messages" that
//! works without device-held material, and therefore no request an administrator
//! or a court can serve on Migo that produces someone's plaintext.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use migo_core::Random;
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};

/// Length of an Ed25519 or X25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of the published identity: signing key followed by exchange key.
pub const IDENTITY_PUBLIC_LEN: usize = PUBLIC_KEY_LEN * 2;
/// Length of an Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

/// Domain separator for a signed prekey.
///
/// Signatures are always over a label plus the data. Without the label, a
/// signature produced for one purpose could be presented as a signature for
/// another — the classic cross-protocol signature confusion.
const PREKEY_DOMAIN: &[u8] = b"migo-signed-prekey-v1";

/// The public half of a device identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityPublic {
    /// Ed25519 verifying key.
    pub signing: [u8; PUBLIC_KEY_LEN],
    /// X25519 public key.
    pub exchange: [u8; PUBLIC_KEY_LEN],
}

impl IdentityPublic {
    /// Serialises to the 64-byte wire form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; IDENTITY_PUBLIC_LEN] {
        let mut out = [0u8; IDENTITY_PUBLIC_LEN];
        out[..PUBLIC_KEY_LEN].copy_from_slice(&self.signing);
        out[PUBLIC_KEY_LEN..].copy_from_slice(&self.exchange);
        out
    }

    /// Parses the 64-byte wire form, rejecting keys that are not valid points.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != IDENTITY_PUBLIC_LEN {
            return Err(CryptoError::BadLength {
                what: "identity public key",
                expected: IDENTITY_PUBLIC_LEN,
                actual: bytes.len(),
            });
        }
        let mut signing = [0u8; PUBLIC_KEY_LEN];
        signing.copy_from_slice(&bytes[..PUBLIC_KEY_LEN]);
        let mut exchange = [0u8; PUBLIC_KEY_LEN];
        exchange.copy_from_slice(&bytes[PUBLIC_KEY_LEN..]);

        // Reject a signing key that is not on the curve here, at parse time,
        // rather than at first use. An invalid key that is stored and only fails
        // later produces a session that cannot be repaired.
        VerifyingKey::from_bytes(&signing).map_err(|_| CryptoError::InvalidPublicKey)?;
        if is_small_order(&exchange) {
            return Err(CryptoError::InvalidPublicKey);
        }
        Ok(Self { signing, exchange })
    }

    /// Verifies a signature made by this identity over `label || message`.
    pub fn verify(&self, label: &[u8], message: &[u8], signature: &[u8]) -> Result<()> {
        let key =
            VerifyingKey::from_bytes(&self.signing).map_err(|_| CryptoError::InvalidPublicKey)?;
        let array: [u8; SIGNATURE_LEN] =
            signature.try_into().map_err(|_| CryptoError::BadLength {
                what: "signature",
                expected: SIGNATURE_LEN,
                actual: signature.len(),
            })?;
        let signature = Signature::from_bytes(&array);
        let mut signed = Vec::with_capacity(label.len() + message.len());
        signed.extend_from_slice(label);
        signed.extend_from_slice(message);
        key.verify(&signed, &signature)
            .map_err(|_| CryptoError::BadSignature)
    }

    /// The 32-byte fingerprint users compare when verifying a contact in person.
    ///
    /// Derived from the full identity rather than one half, so a mismatch in
    /// either key shows up. Rendered as safety numbers by the client.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        crate::kdf::derive(
            &self.to_bytes(),
            Some(b"migo-fingerprint"),
            b"migo-fingerprint-v1",
        )
    }
}

/// The private half of a device identity. Never leaves the device.
pub struct IdentitySecret {
    signing: SigningKey,
    exchange: StaticSecret,
}

impl IdentitySecret {
    /// Generates a new device identity.
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut signing_seed = [0u8; 32];
        random.fill_bytes(&mut signing_seed);
        let mut exchange_seed = [0u8; 32];
        random.fill_bytes(&mut exchange_seed);
        let secret = Self {
            signing: SigningKey::from_bytes(&signing_seed),
            exchange: StaticSecret::from(exchange_seed),
        };
        signing_seed.zeroize();
        exchange_seed.zeroize();
        secret
    }

    /// Rebuilds an identity from its two 32-byte seeds.
    ///
    /// Used by the device's own encrypted storage and by test vectors. The order
    /// is `signing`, then `exchange`, matching the public wire form.
    #[must_use]
    pub fn from_seeds(signing_seed: [u8; 32], exchange_seed: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&signing_seed),
            exchange: StaticSecret::from(exchange_seed),
        }
    }

    /// The public half, for publishing to the server.
    #[must_use]
    pub fn public(&self) -> IdentityPublic {
        IdentityPublic {
            signing: self.signing.verifying_key().to_bytes(),
            exchange: XPublicKey::from(&self.exchange).to_bytes(),
        }
    }

    /// Signs `label || message`.
    #[must_use]
    pub fn sign(&self, label: &[u8], message: &[u8]) -> [u8; SIGNATURE_LEN] {
        let mut signed = Vec::with_capacity(label.len() + message.len());
        signed.extend_from_slice(label);
        signed.extend_from_slice(message);
        self.signing.sign(&signed).to_bytes()
    }

    /// Diffie-Hellman between this identity and a peer's X25519 public key.
    pub(crate) fn diffie_hellman(&self, peer: &[u8; PUBLIC_KEY_LEN]) -> Result<[u8; 32]> {
        if is_small_order(peer) {
            return Err(CryptoError::InvalidPublicKey);
        }
        Ok(self
            .exchange
            .diffie_hellman(&XPublicKey::from(*peer))
            .to_bytes())
    }

    /// Exposes the signing seed, for writing to the device's encrypted store.
    #[must_use]
    pub fn expose_signing_seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Exposes the exchange seed, for writing to the device's encrypted store.
    #[must_use]
    pub fn expose_exchange_seed(&self) -> [u8; 32] {
        self.exchange.to_bytes()
    }
}

impl core::fmt::Debug for IdentitySecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("IdentitySecret(***)")
    }
}

/// An X25519 key pair used as a prekey or a ratchet key.
pub struct KeyPair {
    secret: StaticSecret,
    public: [u8; PUBLIC_KEY_LEN],
}

impl KeyPair {
    /// Generates a fresh pair.
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut seed = [0u8; 32];
        random.fill_bytes(&mut seed);
        let pair = Self::from_seed(seed);
        seed.zeroize();
        pair
    }

    /// Rebuilds a pair from its 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let secret = StaticSecret::from(seed);
        let public = XPublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    /// The public half.
    #[must_use]
    pub fn public(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.public
    }

    /// Diffie-Hellman with a peer's public key.
    pub fn diffie_hellman(&self, peer: &[u8; PUBLIC_KEY_LEN]) -> Result<[u8; 32]> {
        if is_small_order(peer) {
            return Err(CryptoError::InvalidPublicKey);
        }
        Ok(self
            .secret
            .diffie_hellman(&XPublicKey::from(*peer))
            .to_bytes())
    }

    /// Exposes the seed, for the device's encrypted store.
    #[must_use]
    pub fn expose_seed(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }
}

impl core::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &hex(&self.public))
            .finish_non_exhaustive()
    }
}

/// A prekey with the signature that binds it to an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedPrekey {
    /// Identifier the publisher assigned, so a bundle can name which prekey it used.
    pub key_id: u32,
    /// X25519 public key.
    pub public_key: [u8; PUBLIC_KEY_LEN],
    /// Ed25519 signature over the domain label, the key id, and the key.
    pub signature: [u8; SIGNATURE_LEN],
}

impl SignedPrekey {
    /// Signs `pair` with `identity`.
    #[must_use]
    pub fn create(identity: &IdentitySecret, key_id: u32, pair: &KeyPair) -> Self {
        let public_key = pair.public();
        Self {
            key_id,
            public_key,
            signature: identity.sign(PREKEY_DOMAIN, &signed_bytes(key_id, &public_key)),
        }
    }

    /// Verifies that this prekey was signed by `identity`.
    ///
    /// This is the check that makes the server untrusted. The server chooses which
    /// bundle to serve, so without it the server could substitute a prekey it
    /// controls and read everything sent to that device. With it, a substituted
    /// prekey fails verification on the sender's device before any message is
    /// composed.
    pub fn verify(&self, identity: &IdentityPublic) -> Result<()> {
        identity
            .verify(
                PREKEY_DOMAIN,
                &signed_bytes(self.key_id, &self.public_key),
                &self.signature,
            )
            .map_err(|_| CryptoError::InvalidPrekeyBundle)
    }
}

/// The bytes covered by a prekey signature: key id (big-endian) then key.
///
/// The id is inside the signature so that a valid signature cannot be moved onto
/// a different id and cause the two sides to disagree about which prekey was used.
fn signed_bytes(key_id: u32, public_key: &[u8; PUBLIC_KEY_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + PUBLIC_KEY_LEN);
    out.extend_from_slice(&key_id.to_be_bytes());
    out.extend_from_slice(public_key);
    out
}

/// Rejects the known small-order X25519 points.
///
/// `x25519-dalek` already returns an all-zero shared secret for these, so the
/// practical risk is low, but an all-zero secret that is silently accepted means
/// both sides derive the same key from nothing — which is indistinguishable from
/// a working session until someone notices the ciphertext is decryptable by
/// anyone. Rejecting the input is clearer than checking the output.
fn is_small_order(public_key: &[u8; PUBLIC_KEY_LEN]) -> bool {
    /// The complete list from RFC 7748 section 6.1 and Curve25519 analysis.
    const SMALL_ORDER: [[u8; 32]; 7] = [
        [0; 32],
        [
            1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ],
        [
            224, 235, 122, 124, 59, 65, 184, 174, 22, 86, 227, 250, 241, 159, 196, 106, 218, 9,
            141, 235, 156, 50, 177, 253, 134, 98, 5, 22, 95, 73, 184, 0,
        ],
        [
            95, 156, 149, 188, 163, 80, 140, 36, 177, 208, 177, 85, 156, 131, 239, 91, 4, 68, 92,
            196, 88, 28, 142, 134, 216, 34, 78, 221, 208, 159, 17, 87,
        ],
        [
            236, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 127,
        ],
        [
            237, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 127,
        ],
        [
            238, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 127,
        ],
    ];

    // Constant-time over the whole list: a timing difference here would leak
    // which candidate matched, and there is no reason to accept that.
    let mut matched = 0u8;
    for candidate in &SMALL_ORDER {
        matched |= public_key.ct_eq(candidate).unwrap_u8();
    }
    matched != 0
}

/// Lowercase hex, for `Debug` of public material only.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    fn identity(seed: u64) -> IdentitySecret {
        let mut random = SeededRandom::new(seed);
        IdentitySecret::generate(&mut random)
    }

    #[test]
    fn a_public_identity_round_trips_through_its_wire_form() {
        let public = identity(1).public();
        let parsed = IdentityPublic::parse(&public.to_bytes()).expect("parses");
        assert_eq!(parsed, public);
    }

    #[test]
    fn the_wire_form_is_signing_then_exchange() {
        let public = identity(2).public();
        let bytes = public.to_bytes();
        assert_eq!(&bytes[..32], &public.signing);
        assert_eq!(&bytes[32..], &public.exchange);
    }

    #[test]
    fn a_wrong_length_identity_is_rejected() {
        assert!(matches!(
            IdentityPublic::parse(&[0u8; 32]),
            Err(CryptoError::BadLength {
                expected: 64,
                actual: 32,
                ..
            })
        ));
    }

    #[test]
    fn signatures_verify_and_are_domain_separated() {
        let secret = identity(3);
        let public = secret.public();
        let signature = secret.sign(b"domain-a", b"message");
        public
            .verify(b"domain-a", b"message", &signature)
            .expect("verifies");
        assert_eq!(
            public.verify(b"domain-b", b"message", &signature),
            Err(CryptoError::BadSignature),
            "a signature must not be reusable under another domain"
        );
    }

    #[test]
    fn another_identity_cannot_verify() {
        let signature = identity(4).sign(b"d", b"m");
        assert_eq!(
            identity(5).public().verify(b"d", b"m", &signature),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let secret = identity(6);
        let mut signature = secret.sign(b"d", b"m");
        signature[0] ^= 1;
        assert_eq!(
            secret.public().verify(b"d", b"m", &signature),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn diffie_hellman_agrees_in_both_directions() {
        let alice = identity(7);
        let bob = identity(8);
        let from_alice = alice.diffie_hellman(&bob.public().exchange).expect("dh");
        let from_bob = bob.diffie_hellman(&alice.public().exchange).expect("dh");
        assert_eq!(from_alice, from_bob);
    }

    #[test]
    fn small_order_public_keys_are_refused() {
        let alice = identity(9);
        assert_eq!(
            alice.diffie_hellman(&[0u8; 32]),
            Err(CryptoError::InvalidPublicKey)
        );
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(
            alice.diffie_hellman(&one),
            Err(CryptoError::InvalidPublicKey)
        );
    }

    #[test]
    fn a_small_order_key_cannot_be_published_as_an_identity() {
        let mut bytes = identity(10).public().to_bytes();
        bytes[32..].copy_from_slice(&[0u8; 32]);
        assert_eq!(
            IdentityPublic::parse(&bytes),
            Err(CryptoError::InvalidPublicKey)
        );
    }

    #[test]
    fn seeds_reconstruct_the_same_identity() {
        let original = identity(11);
        let restored = IdentitySecret::from_seeds(
            original.expose_signing_seed(),
            original.expose_exchange_seed(),
        );
        assert_eq!(restored.public(), original.public());
    }

    #[test]
    fn a_signed_prekey_verifies_against_its_identity() {
        let mut random = SeededRandom::new(12);
        let secret = IdentitySecret::generate(&mut random);
        let pair = KeyPair::generate(&mut random);
        let prekey = SignedPrekey::create(&secret, 42, &pair);
        prekey.verify(&secret.public()).expect("verifies");
    }

    #[test]
    fn a_prekey_from_another_identity_is_refused() {
        // The substituted-prekey attack: the server serves a prekey it controls.
        let mut random = SeededRandom::new(13);
        let real = IdentitySecret::generate(&mut random);
        let attacker = IdentitySecret::generate(&mut random);
        let attacker_pair = KeyPair::generate(&mut random);
        let forged = SignedPrekey::create(&attacker, 1, &attacker_pair);
        assert_eq!(
            forged.verify(&real.public()),
            Err(CryptoError::InvalidPrekeyBundle)
        );
    }

    #[test]
    fn a_prekey_signature_does_not_transfer_to_another_key_id() {
        let mut random = SeededRandom::new(14);
        let secret = IdentitySecret::generate(&mut random);
        let pair = KeyPair::generate(&mut random);
        let mut prekey = SignedPrekey::create(&secret, 1, &pair);
        prekey.key_id = 2;
        assert_eq!(
            prekey.verify(&secret.public()),
            Err(CryptoError::InvalidPrekeyBundle)
        );
    }

    #[test]
    fn a_prekey_signature_does_not_transfer_to_another_key() {
        let mut random = SeededRandom::new(15);
        let secret = IdentitySecret::generate(&mut random);
        let pair = KeyPair::generate(&mut random);
        let other = KeyPair::generate(&mut random);
        let mut prekey = SignedPrekey::create(&secret, 1, &pair);
        prekey.public_key = other.public();
        assert_eq!(
            prekey.verify(&secret.public()),
            Err(CryptoError::InvalidPrekeyBundle)
        );
    }

    #[test]
    fn fingerprints_differ_between_identities_and_are_stable() {
        let a = identity(16).public();
        let b = identity(17).public();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), a.fingerprint());
    }

    #[test]
    fn secrets_do_not_print_themselves() {
        let secret = identity(18);
        assert_eq!(format!("{secret:?}"), "IdentitySecret(***)");
        let mut random = SeededRandom::new(19);
        let pair = KeyPair::generate(&mut random);
        let rendered = format!("{pair:?}");
        assert!(rendered.contains("public"), "{rendered}");
        assert!(
            !rendered.contains(&hex(&pair.expose_seed())),
            "a seed appeared in Debug output"
        );
    }
}
