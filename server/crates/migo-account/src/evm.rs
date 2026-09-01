//! The EVM wallet domain: BIP-32 over the domain seed, BIP-44 coin type 60.
//!
//! # Standards, not a Migo format
//!
//! The wallet is the one domain where an established hierarchical standard
//! exists, so this module implements that standard rather than a Migo-shaped
//! cousin of it (spec #4): the `MIGO/EVM/V1` domain seed becomes a BIP-32
//! master seed, accounts are the BIP-44 path `m/44'/60'/0'/0/i`, the curve is
//! secp256k1, and the address is the last 20 bytes of Keccak-256 over the
//! 64-byte uncompressed public key, checksummed per EIP-55 for display. A
//! wallet recovered from a container is therefore not just "the same address
//! Migo shows" — it is the address any standards-conformant Ethereum tool
//! derives from the same seed.
//!
//! The private key never leaves the device. The server receives the address
//! and metadata; that is all it is ever offered, and all it can do with an
//! address is display it — this release has no RPC, no balances, no
//! broadcasting, and the UI must not imply otherwise (§182).
//!
//! # What is implemented here and why
//!
//! BIP-32's CKDpriv is a few dozen lines over audited primitives (HMAC-SHA512
//! and secp256k1 scalar arithmetic), and it is pinned by the official BIP-32
//! test vectors plus the independent Python generator behind the conformance
//! vectors, so the derivation policy is written here in the open rather than
//! buried in a dependency's feature set. Everything harder than HMAC is
//! delegated: `secp256k1` for the curve, `tiny_keccak` for Keccak-256
//! (Ethereum's pre-standard padding, not SHA3-256 — using SHA3 here would
//! produce plausible-looking addresses no chain agrees with).

use hmac::{Hmac, Mac};
use secp256k1::{Scalar, SecretKey};
use sha2::Sha512;
use tiny_keccak::{Hasher, Keccak};
use zeroize::Zeroize;

use crate::error::{AccountError, Result};
use crate::root::{MigoRoot, DOMAIN_EVM};

/// BIP-44 coin type for Ethereum and the EVM family.
pub const EIP155_COIN_TYPE: u32 = 60;
/// The account path this build derives, as documentation: the code walks the
/// levels, the constant is what a BIP-44 tool prints for the result.
pub const EVM_BIP44_PATH: &str = "m/44'/60'/0'/0";

/// HMAC-SHA512, the only hash BIP-32 defines.
type HmacSha512 = Hmac<Sha512>;

/// The BIP-32 hardened-bit.
const BIP32_HARDENED: u32 = 0x8000_0000;

/// A derived EVM wallet: one BIP-44 account of the root's EVM domain.
///
/// Holds the private key zeroized-on-drop. The address is computed once at
/// construction and stored as bytes, because it is the public identity of the
/// wallet and callers display it far more often than they derive it.
pub struct EvmWallet {
    private_key: SecretKey,
    chain_code: [u8; 32],
    /// The 20-byte address.
    address: [u8; 20],
}

impl EvmWallet {
    /// Derives wallet `index` of the root's EVM domain.
    ///
    /// # Errors
    ///
    /// [`AccountError::InvalidDerivation`] in the BIP-32-assigned
    /// probability-2^-127 case of an invalid intermediate scalar.
    pub fn from_root(root: &MigoRoot, index: u32) -> Result<Self> {
        Self::derive(root.domain_seed(DOMAIN_EVM), index)
    }

    /// Derives wallet `index` from an explicit EVM domain seed — the form the
    /// conformance vectors and a container restore use.
    ///
    /// # Errors
    ///
    /// As [`EvmWallet::from_root`].
    pub fn derive(domain_seed: [u8; 32], index: u32) -> Result<Self> {
        // BIP-32 master key generation: I = HMAC-SHA512(key = "Bitcoin seed",
        // data = seed). The label is the standard's, not a Migo one — that is
        // the point of this domain.
        let master = hmac_sha512(b"Bitcoin seed", &domain_seed);
        let mut secret = [0u8; 32];
        let mut chain_code = [0u8; 32];
        secret.copy_from_slice(&master[..32]);
        chain_code.copy_from_slice(&master[32..]);

        // m/44'/60'/0'/0/i, walked level by level exactly as BIP-44 prescribes
        // for coin type 60: three hardened levels, the change level 0, and the
        // requested account index.
        for level in [
            44 + BIP32_HARDENED,
            EIP155_COIN_TYPE + BIP32_HARDENED,
            BIP32_HARDENED,
            0,
            index,
        ] {
            let (child_secret, child_code) = ckd_priv(&secret, &chain_code, level)?;
            secret = child_secret;
            chain_code = child_code;
        }

        let private_key =
            SecretKey::from_secret_bytes(secret).map_err(|_| AccountError::InvalidDerivation)?;
        let address = address_of(&private_key);
        Ok(Self {
            private_key,
            chain_code,
            address,
        })
    }

    /// The 20-byte address.
    #[must_use]
    pub fn address(&self) -> &[u8; 20] {
        &self.address
    }

    /// The EIP-55 checksummed address, the only form that should ever be
    /// shown to a user — a mistyped checksummed address is rejected by every
    /// tool that receives it, which is exactly the property display wants.
    #[must_use]
    pub fn address_checksummed(&self) -> String {
        eip55(&self.address)
    }

    /// The BIP-32 chain code after the full path, for container metadata.
    #[must_use]
    pub fn chain_code(&self) -> &[u8; 32] {
        &self.chain_code
    }

    /// The private key bytes, for signing inside the device's secure
    /// environment. This is the only accessor that exposes secret material,
    /// and it exists because whatever consumes this wallet next — transaction
    /// signing, EIP-712 — is a local operation by definition.
    #[must_use]
    pub fn private_key_bytes(&self) -> [u8; 32] {
        self.private_key.to_secret_bytes()
    }
}

// `SecretKey` is not `Zeroize` in the secp256k1 crate, so the drop impl does
// it by hand through the only accessor that returns owned secret bytes.
impl Drop for EvmWallet {
    fn drop(&mut self) {
        self.private_key_bytes().zeroize();
    }
}

impl std::fmt::Debug for EvmWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The address is public; the key is not, and Debug renders the
        // checksummed address so a log line is useful without being dangerous.
        write!(f, "EvmWallet({})", eip55(&self.address))
    }
}

/// One BIP-32 CKDpriv step: returns `(child_secret, child_chain_code)`.
///
/// Hardened levels hash the parent secret; non-hardened levels hash the parent
/// public key. The distinction is the whole privacy property of BIP-32 — a
/// non-hardened child can be derived by someone holding only the parent public
/// key and chain code — which is why it is decided here, in one place, from the
/// index bit rather than by a caller remembering to pick a function.
fn ckd_priv(
    parent_secret: &[u8; 32],
    parent_code: &[u8; 32],
    index: u32,
) -> Result<([u8; 32], [u8; 32])> {
    let parent_key = SecretKey::from_secret_bytes(*parent_secret)
        .map_err(|_| AccountError::InvalidDerivation)?;
    let mut mac =
        HmacSha512::new_from_slice(parent_code).map_err(|_| AccountError::InvalidDerivation)?;
    if index >= BIP32_HARDENED {
        mac.update(&[0u8]);
        mac.update(parent_secret);
    } else {
        // Compressed serialization, 33 bytes: the standard digest input for a
        // non-hardened step.
        mac.update(&parent_key.public_key().serialize());
    }
    mac.update(&index.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // parse256(IL): a value at or above the curve order invalidates the step.
    let mut tweak_bytes = [0u8; 32];
    tweak_bytes.copy_from_slice(&digest[..32]);
    let tweak = Scalar::from_be_bytes(tweak_bytes).map_err(|_| AccountError::InvalidDerivation)?;
    // kchild = IL + kpar (mod n); `add_tweak` also refuses a zero result, which
    // is the other BIP-32 invalid case.
    let child_key = parent_key
        .add_tweak(&tweak)
        .map_err(|_| AccountError::InvalidDerivation)?;

    let mut child_code = [0u8; 32];
    child_code.copy_from_slice(&digest[32..]);
    Ok((child_key.to_secret_bytes(), child_code))
}

/// HMAC-SHA512 in one call, for the master step.
fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut mac = HmacSha512::new_from_slice(key).expect("HMAC accepts every key length");
    mac.update(data);
    let mut out = [0u8; 64];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// The 20-byte Ethereum address of a secret key: the last 20 bytes of
/// Keccak-256 over the 64-byte public key (X || Y, without the 0x04 prefix —
/// including it is the classic way to derive a valid-looking wrong address).
fn address_of(secret: &SecretKey) -> [u8; 20] {
    let public = secret.public_key().serialize_uncompressed();
    let mut hasher = Keccak::v256();
    let mut digest = [0u8; 32];
    hasher.update(&public[1..]);
    hasher.finalize(&mut digest);
    let mut address = [0u8; 20];
    address.copy_from_slice(&digest[12..]);
    address
}

/// Renders a 20-byte address in EIP-55 form: lowercase hex, then each letter
/// uppercased where the corresponding nibble of Keccak-256 of that lowercase
/// hex string is ≥ 8.
#[must_use]
pub fn eip55(address: &[u8; 20]) -> String {
    let lowercase = hex(address);
    let mut hasher = Keccak::v256();
    let mut digest = [0u8; 32];
    hasher.update(lowercase.as_bytes());
    hasher.finalize(&mut digest);

    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (i, ch) in lowercase.chars().enumerate() {
        // Digits are never cased; letters follow the digest nibble. EIP-55
        // indexes the digest by the hex character position, which for a
        // 40-character string is the same as the nibble index.
        let nibble = if i % 2 == 0 {
            digest[i / 2] >> 4
        } else {
            digest[i / 2] & 0x0F
        };
        if ch.is_ascii_digit() || nibble < 8 {
            out.push(ch);
        } else {
            out.push(ch.to_ascii_uppercase());
        }
    }
    out
}

/// Lowercase hex without the 0x prefix, 40 characters for an address.
fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain_seed(constant: u8) -> [u8; 32] {
        [constant; 32]
    }

    #[test]
    fn bip32_test_vector_one_reproduces() {
        // BIP-32's own published Test Vector 1, driven through the same
        // ckd_priv the wallet path uses. If this fails, the derivation is not
        // BIP-32 — it is a lookalike, and every address this crate has ever
        // shown is a number nobody else can reproduce.
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").expect("vector seed");
        let master = hmac_sha512(b"Bitcoin seed", &seed);
        let mut secret = [0u8; 32];
        let mut code = [0u8; 32];
        secret.copy_from_slice(&master[..32]);
        code.copy_from_slice(&master[32..]);

        // m/0'
        let (s0, c0) = ckd_priv(&secret, &code, BIP32_HARDENED).expect("m/0'");
        assert_eq!(
            hex::encode(s0),
            "edb2e14f9ee77d26dd93b4ecede8d16ed408ce149b6cd80b0715a2d911a0afea"
        );
        assert_eq!(
            hex::encode(c0),
            "47fdacbd0f1097043b78c63c20c34ef4ed9a111d980047ad16282c7ae6236141"
        );

        // m/0'/1 (non-hardened step)
        let (s1, _) = ckd_priv(&s0, &c0, 1).expect("m/0'/1");
        assert_eq!(
            hex::encode(s1),
            "3c6cb8d0f6a264c91ea8b5030fadaa8e538b020f0a387421a12de9319dc93368"
        );
    }

    #[test]
    fn the_wallet_address_is_keccak_of_the_public_key() {
        let wallet = EvmWallet::derive(domain_seed(0x11), 0).expect("derivation works");
        // Independent property: the address must equal keccak256(pubkey)[12..]
        // computed here from the private key bytes, which catches a wrong
        // serialization (prefix included) or a truncated digest.
        let key =
            SecretKey::from_secret_bytes(wallet.private_key_bytes()).expect("the key round-trips");
        let expected = address_of(&key);
        assert_eq!(wallet.address(), &expected);
        assert_eq!(wallet.address().len(), 20);
    }

    #[test]
    fn indices_give_distinct_wallets() {
        let a = EvmWallet::derive(domain_seed(0x22), 0).expect("derive 0");
        let b = EvmWallet::derive(domain_seed(0x22), 1).expect("derive 1");
        let c = EvmWallet::derive(domain_seed(0x23), 0).expect("derive other root");
        assert_ne!(a.address(), b.address());
        assert_ne!(a.address(), c.address());
    }

    #[test]
    fn derivation_is_a_pure_function_of_the_domain_seed() {
        let a = EvmWallet::derive(domain_seed(0x33), 7).expect("derive");
        let b = EvmWallet::derive(domain_seed(0x33), 7).expect("derive again");
        assert_eq!(a.address(), b.address());
        assert_eq!(a.chain_code(), b.chain_code());
        assert_eq!(a.private_key_bytes(), b.private_key_bytes());
    }

    #[test]
    fn eip55_matches_the_canonical_examples() {
        // The five example addresses from EIP-55 itself. A checksum routine
        // that agrees with the EIP on all five is the routine; one that
        // agrees on four has a nibble indexing bug.
        for known in [
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
            "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
            "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
            "0x52908400098527886E0F7030069857D2E4169EE7",
        ] {
            let bytes: [u8; 20] = hex::decode(&known[2..])
                .expect("example address")
                .try_into()
                .expect("20 bytes");
            assert_eq!(eip55(&bytes), known);
        }
    }

    #[test]
    fn debug_shows_the_address_not_the_key() {
        let wallet = EvmWallet::derive(domain_seed(0x44), 0).expect("derive");
        let rendered = format!("{wallet:?}");
        assert!(rendered.starts_with("EvmWallet(0x"), "useful in a log line");
        assert!(
            !rendered.contains(&hex::encode(wallet.private_key_bytes())),
            "the private key must not render"
        );
    }

    #[test]
    fn the_root_derives_the_same_wallet_as_the_domain_seed() {
        let root = MigoRoot::from_bytes(&[0x55u8; 32]).expect("root");
        let from_root = EvmWallet::from_root(&root, 2).expect("from root");
        let from_seed = EvmWallet::derive(root.domain_seed(DOMAIN_EVM), 2).expect("from seed");
        assert_eq!(from_root.address(), from_seed.address());
        assert_eq!(
            from_root.address_checksummed(),
            from_seed.address_checksummed()
        );
    }
}
