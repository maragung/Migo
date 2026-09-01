//! The `.migo` container: the whole account, portable and encrypted.
//!
//! # What the file is
//!
//! One root secret is the entire account, so one sealed blob is the entire
//! backup: the root, a format version, and a creation timestamp, encrypted
//! under a key derived from a *recovery credential* the user chose — not their
//! password, not their e-mail, not their Google account (§182). The file is
//! named `.migo`, copied to Google Drive or a USB stick by the user, and
//! holds only ciphertext: a container in a cloud bucket is Argon2id work for
//! whoever steals the bucket, and nothing else.
//!
//! # The header
//!
//! ```text
//! "MIGOACCT1"  9 bytes magic
//! u16 BE       format version (1)
//! u16 BE       crypto version (1)
//! u8           KDF id (1 = Argon2id)
//! u32 BE       Argon2id memory cost, KiB
//! u32 BE       Argon2id time cost, passes
//! u32 BE       Argon2id lanes
//! 16 bytes     Argon2id salt
//! 24 bytes     XChaCha20-Poly1305 nonce
//! remainder    sealed body: ciphertext || tag
//! ```
//!
//! 66 bytes of header, and the whole header is the AEAD's associated data:
//! editing the salt, lowering the stored cost, or swapping a nonce between
//! files breaks the tag before any of it is used. The Argon2id parameters
//! ride in the file (big-endian, unlike the vault's little-endian — the
//! container is a cross-port format and both ports read the field the same
//! way) so raising the cost for new containers never locks out an old one.
//!
//! # The key schedule
//!
//! The recovery credential is stretched by Argon2id at the header's
//! parameters, then the 32-byte result goes through HKDF under the
//! `MIGO/BACKUP/V1` label before it encrypts anything. The extra HKDF step
//! costs nothing and keeps the promise that the root's own domain labels are
//! the only derivations in the system: the backup key is a *cousin* of the
//! Argon2 output, not the Argon2 output itself, so a hypothetical weakness
//! in one never lands directly on the other.
//!
//! # One error for everything
//!
//! A wrong credential, a tampered byte, and a truncated file all fail with
//! [`AccountError::OpenFailed`] — the container reader cannot distinguish
//! them, so it must not tell the caller which happened (§182). The only
//! distinct errors are the ones that name a *remedy*: a newer format version
//! means "update the app", an unknown KDF id means "this file is from a
//! future build", and parameters out of range mean the header was never
//! written by this code.

use argon2::{Algorithm, Argon2, Params, Version};
use migo_core::Random;
use migo_crypto::aead;
use migo_crypto::kdf;
use migo_crypto::SymmetricKey;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::{AccountError, Result};
use crate::root::{MigoRoot, DOMAIN_BACKUP, ROOT_LEN};

/// The container magic. The trailing digit is the format generation: a file
/// this build refuses to read is a file whose magic it does not carry.
pub const MAGIC: &[u8; 9] = b"MIGOACCT1";
/// The format version this build writes and reads.
pub const FORMAT_VERSION: u16 = 1;
/// The crypto version this build writes and reads. Bumped when the key
/// schedule or AEAD changes, independently of payload changes.
pub const CRYPTO_VERSION: u16 = 1;
/// Argon2id, the only KDF id this build understands.
pub const KDF_ARGON2ID: u8 = 1;

/// Argon2id salt length in bytes.
pub const SALT_LEN: usize = 16;
/// The AEAD nonce length in bytes (XChaCha20-Poly1305).
pub const NONCE_LEN: usize = aead::NONCE_LEN;
/// Total header length: magic, two versions, KDF id, three cost words, salt,
/// nonce.
pub const HEADER_LEN: usize = MAGIC.len() + 2 + 2 + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN;

/// Argon2id memory cost for new containers, in KiB: 64 MiB, matching the
/// desktop vault — an offline grind against a stolen container should cost
/// the same as one against a stolen vault.
pub const MEMORY_KIB: u32 = 64 * 1024;
/// Argon2id passes for new containers.
pub const TIME_COST: u32 = 3;
/// Argon2id lanes for new containers.
pub const LANES: u32 = 1;

/// Shortest recovery credential accepted. Same reasoning as the vault
/// passphrase: length is the rule, composition rules push people towards
/// dictionary words.
pub const MIN_CREDENTIAL_BYTES: usize = 8;
/// Longest recovery credential accepted, so a pasted file cannot turn one
/// open into a minute of hashing.
pub const MAX_CREDENTIAL_BYTES: usize = 1024;

/// The Argon2id parameters, read from a header or chosen for a new container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerParams {
    /// Memory cost, KiB.
    pub memory_kib: u32,
    /// Time cost, passes.
    pub time_cost: u32,
    /// Lanes.
    pub lanes: u32,
}

impl ContainerParams {
    /// The parameters new containers are sealed with.
    pub const CURRENT: Self = Self {
        memory_kib: MEMORY_KIB,
        time_cost: TIME_COST,
        lanes: LANES,
    };

    /// Rejects parameters this build will not spend memory on.
    ///
    /// A stored cost is attacker-controlled input in the sense that anyone
    /// who can write the file can set it. The tag over the header already
    /// stops a silent downgrade, but a floor here means a hostile container
    /// naming 4 GiB of Argon2 memory is refused *before* the allocation, not
    /// after the process has been evicted.
    ///
    /// # Errors
    ///
    /// [`AccountError::KdfOutOfRange`] outside 8 MiB..4 GiB of memory, or
    /// passes/lanes outside 1..=16.
    pub fn validate(self) -> Result<Self> {
        let sane = (8 * 1024..=4 * 1024 * 1024).contains(&self.memory_kib)
            && (1..=16).contains(&self.time_cost)
            && (1..=16).contains(&self.lanes);
        if sane {
            Ok(self)
        } else {
            Err(AccountError::KdfOutOfRange)
        }
    }
}

/// The decrypted container payload: everything a new device needs to become
/// the account again.
///
/// Deliberately small. The root is the account; metadata exists so a future
/// reader can tell what it is holding without decrypting the whole history of
/// format changes. Wallet addresses and device lists are *not* here — they
/// are functions of the root or live on the server, and duplicating them into
/// the backup would create a second copy that can drift from the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountFile {
    /// The account payload format version. Bumped when fields change;
    /// readers accept exactly one generation per build.
    pub version: u16,
    /// When this container was sealed, Unix seconds. Display material, not
    /// security material.
    pub created_at: u64,
    /// The root secret, hex-encoded: 64 characters. The only secret in the
    /// file, and the only one any account needs.
    pub root: String,
    /// The account's server-side id, in its text form, when the sealing
    /// device knew it.
    ///
    /// The restore ceremony (`POST /v1/auth/identity/challenge` with purpose
    /// `add-device`) names the account being restored, and the container is
    /// where that name belongs: the sealing device learned it from the grant
    /// it signed in with, and a restoring device has nothing else to say it
    /// with. It is deliberately the *last* field and deliberately optional —
    /// containers sealed before the field existed, and the conformance
    /// vectors that pin this file's bytes, serialise exactly the three
    /// fields above, and `skip_serializing_if` keeps those bytes unchanged.
    /// A restoring device that finds `None` here cannot run the ceremony and
    /// says so, rather than guessing at an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl AccountFile {
    /// Builds a payload for `root`, stamped `now` (Unix seconds).
    #[must_use]
    pub fn new(root: &MigoRoot, now: u64) -> Self {
        Self {
            version: FORMAT_VERSION,
            created_at: now,
            root: hex(root.as_bytes()),
            account_id: None,
        }
    }

    /// Names the account this container restores, from the grant the sealing
    /// device signed in with.
    #[must_use]
    pub fn for_account(mut self, account_id: &str) -> Self {
        self.account_id = Some(account_id.to_owned());
        self
    }

    /// The root secret.
    ///
    /// # Errors
    ///
    /// [`AccountError::BadLength`] if the hex does not decode to 32 bytes,
    /// which for a payload that passed the AEAD tag means the container was
    /// written by something else that shares the format.
    pub fn root(&self) -> Result<MigoRoot> {
        let decoded = unhex(&self.root).ok_or(AccountError::BadLength {
            what: "container root",
            expected: ROOT_LEN,
            actual: self.root.len() / 2,
        })?;
        MigoRoot::from_bytes(&decoded)
    }
}

/// Seals an account into container bytes with fresh salt and nonce.
///
/// # Errors
///
/// [`AccountError::BadLength`] if the credential is outside
/// MIN_CREDENTIAL_BYTES..=MAX_CREDENTIAL_BYTES, and whatever
/// [`seal_container_with`] reports otherwise.
pub fn seal_container(
    credential: &str,
    file: &AccountFile,
    random: &mut dyn Random,
) -> Result<Vec<u8>> {
    let mut salt = [0u8; SALT_LEN];
    random.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    random.fill_bytes(&mut nonce);
    seal_container_with(credential, file, ContainerParams::CURRENT, &salt, &nonce)
}

/// Seals with caller-supplied salt and nonce: the deterministic form the
/// conformance vectors use. Application code wants [`seal_container`], whose
/// random salt and nonce make every container unique even for the identical
/// account and credential.
///
/// # Errors
///
/// [`AccountError::BadLength`] for a bad credential length,
/// [`AccountError::KdfOutOfRange`] for parameters out of range.
pub fn seal_container_with(
    credential: &str,
    file: &AccountFile,
    params: ContainerParams,
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>> {
    check_credential(credential)?;
    params.validate()?;

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    header.extend_from_slice(&CRYPTO_VERSION.to_be_bytes());
    header.push(KDF_ARGON2ID);
    header.extend_from_slice(&params.memory_kib.to_be_bytes());
    header.extend_from_slice(&params.time_cost.to_be_bytes());
    header.extend_from_slice(&params.lanes.to_be_bytes());
    header.extend_from_slice(salt);
    header.extend_from_slice(nonce);
    debug_assert_eq!(header.len(), HEADER_LEN);

    let key = container_key(credential, salt, params)?;
    let mut plaintext = serde_json::to_vec(file).map_err(|_| AccountError::OpenFailed)?;
    // `seal_with_nonce` returns nonce || ciphertext || tag, which is the body
    // this format stores: readers hand the whole body to `aead::open`.
    let body = aead::seal_with_nonce(&key, nonce, &header, &plaintext)
        .map_err(|_| AccountError::OpenFailed)?;
    plaintext.zeroize();

    header.extend_from_slice(&body);
    Ok(header)
}

/// Opens a container: verifies the header, derives the key, decrypts, and
/// returns the account.
///
/// # Errors
///
/// [`AccountError::NotAContainer`] for a file that is not one (wrong magic or
/// shorter than a header). [`AccountError::UnsupportedVersion`] for a format
/// or crypto version this build does not read — the honest remedy there is
/// updating the app, not retrying the credential. [`AccountError::UnknownKdf`]
/// for a KDF id this build does not implement. [`AccountError::OpenFailed`]
/// for everything else: wrong credential, tampered bytes, or a payload that
/// is not an account — without saying which.
pub fn open_container(credential: &str, bytes: &[u8]) -> Result<AccountFile> {
    check_credential(credential)?;
    if bytes.len() < HEADER_LEN {
        return Err(AccountError::NotAContainer);
    }
    let (header, body) = bytes.split_at(HEADER_LEN);
    if &header[..MAGIC.len()] != MAGIC {
        return Err(AccountError::NotAContainer);
    }
    let mut cursor = MAGIC.len();
    let format_version = u16::from_be_bytes(
        header[cursor..cursor + 2]
            .try_into()
            .expect("the slice is two bytes"),
    );
    cursor += 2;
    let crypto_version = u16::from_be_bytes(
        header[cursor..cursor + 2]
            .try_into()
            .expect("the slice is two bytes"),
    );
    cursor += 2;
    if format_version != FORMAT_VERSION || crypto_version != CRYPTO_VERSION {
        // A container from a future build: refuse rather than guess at what
        // its fields mean. Guessing wrong at this layer can corrupt the only
        // copy of someone's account.
        return Err(AccountError::UnsupportedVersion {
            found: format_version.max(crypto_version),
            supported: FORMAT_VERSION,
        });
    }
    let kdf_id = header[cursor];
    cursor += 1;
    if kdf_id != KDF_ARGON2ID {
        return Err(AccountError::UnknownKdf { found: kdf_id });
    }
    let memory_kib = u32::from_be_bytes(
        header[cursor..cursor + 4]
            .try_into()
            .expect("the slice is four bytes"),
    );
    cursor += 4;
    let time_cost = u32::from_be_bytes(
        header[cursor..cursor + 4]
            .try_into()
            .expect("the slice is four bytes"),
    );
    cursor += 4;
    let lanes = u32::from_be_bytes(
        header[cursor..cursor + 4]
            .try_into()
            .expect("the slice is four bytes"),
    );
    cursor += 4;
    let params = ContainerParams {
        memory_kib,
        time_cost,
        lanes,
    }
    .validate()?;
    let salt: [u8; SALT_LEN] = header[cursor..cursor + SALT_LEN]
        .try_into()
        .expect("the slice is SALT_LEN bytes");
    cursor += SALT_LEN;
    // The header's nonce is advisory for readers that parse it field by
    // field; the body carries the authoritative copy as its prefix, and the
    // two must agree or the tag fails — swapping a header between files is
    // the attack that arrangement closes.
    let _ = &header[cursor..cursor + NONCE_LEN];

    let key = container_key(credential, &salt, params)?;
    let plaintext = aead::open(&key, header, body).map_err(|_| AccountError::OpenFailed)?;
    // Everything below fails as OpenFailed on purpose: a payload that
    // decrypted but is not a readable account is indistinguishable from a
    // wrong credential as far as any caller needs to know.
    let file =
        serde_json::from_slice::<AccountFile>(&plaintext).map_err(|_| AccountError::OpenFailed)?;
    let mut plaintext = plaintext;
    plaintext.zeroize();
    file.root()?;
    Ok(file)
}

/// Argon2id, then HKDF under the backup domain label.
fn container_key(
    credential: &str,
    salt: &[u8; SALT_LEN],
    params: ContainerParams,
) -> Result<SymmetricKey> {
    let argon_params = Params::new(params.memory_kib, params.time_cost, params.lanes, Some(32))
        .map_err(|_| AccountError::KdfOutOfRange)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut stretched = [0u8; 32];
    argon
        .hash_password_into(credential.as_bytes(), salt, &mut stretched)
        .map_err(|_| AccountError::OpenFailed)?;
    let derived = kdf::derive::<32>(&stretched, None, DOMAIN_BACKUP);
    stretched.zeroize();
    Ok(SymmetricKey::from_bytes(derived))
}

fn check_credential(credential: &str) -> Result<()> {
    let length = credential.len();
    if (MIN_CREDENTIAL_BYTES..=MAX_CREDENTIAL_BYTES).contains(&length) {
        Ok(())
    } else {
        Err(AccountError::BadLength {
            what: "recovery credential",
            expected: MIN_CREDENTIAL_BYTES,
            actual: length,
        })
    }
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0F) as usize] as char);
    }
    out
}

/// Hex decode that rejects odd lengths and non-hex characters — a container
/// payload is either exactly right or it is not an account.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root::MigoRoot;
    use migo_core::OsRandom;

    /// Fast parameters inside the accepted floor, so the test suite does not
    /// pay 64 MiB of Argon2 per case.
    fn fast_params() -> ContainerParams {
        ContainerParams {
            memory_kib: 8 * 1024,
            time_cost: 1,
            lanes: 1,
        }
    }

    fn seal_fast(credential: &str, file: &AccountFile) -> Vec<u8> {
        let mut salt = [7u8; SALT_LEN];
        let mut nonce = [9u8; NONCE_LEN];
        salt[0] ^= file.created_at as u8;
        nonce[1] ^= file.version as u8;
        seal_container_with(credential, file, fast_params(), &salt, &nonce)
            .expect("sealing with in-range parameters works")
    }

    fn sample() -> (MigoRoot, AccountFile) {
        let root = MigoRoot::from_bytes(&(0..32u8).collect::<Vec<u8>>()).expect("root");
        let file = AccountFile::new(&root, 1_700_000_000);
        (root, file)
    }

    #[test]
    fn the_round_trip_returns_the_same_root() {
        let (root, file) = sample();
        let container = seal_fast("correct horse battery staple", &file);
        let opened = open_container("correct horse battery staple", &container).expect("opens");
        assert_eq!(opened.root().expect("root"), root);
        assert_eq!(opened.version, file.version);
        assert_eq!(opened.created_at, file.created_at);
        // The account name is optional metadata, and a container sealed
        // without one must round-trip to `None` rather than to a guess.
        assert_eq!(opened.account_id, None);
    }

    #[test]
    fn the_account_name_rides_along_and_the_nameless_bytes_do_not_move() {
        let (root, file) = sample();
        // The three-field form is the byte contract the conformance vectors
        // pin: adding an optional field must not move it by one byte.
        let plain = serde_json::to_string(&file).expect("serialises");
        assert_eq!(
            plain,
            format!(
                "{{\"version\":1,\"created_at\":{},\"root\":\"{}\"}}",
                file.created_at, file.root
            )
        );

        // A named container round-trips the account id through the seal.
        let named = file.for_account("01j8y0migo0migo0migo0migo0migo");
        let container = seal_fast("credential", &named);
        let opened = open_container("credential", &container).expect("opens");
        assert_eq!(opened.account_id.as_deref(), Some("01j8y0migo0migo0migo0migo0migo"));

        // And a reader that does not know the field still opens the named
        // container, because serde ignores unknown keys by default — the
        // forward-compatibility rule every port already follows.
        let named_bytes = serde_json::to_vec(&named).expect("serialises");
        let decoded: AccountFile = serde_json::from_slice(&named_bytes).expect("parses");
        assert_eq!(decoded, named);
    }

    #[test]
    fn the_header_is_sixty_six_bytes_and_laid_out_as_documented() {
        let (_root, file) = sample();
        let container = seal_fast("credential", &file);
        assert_eq!(HEADER_LEN, 66);
        assert_eq!(&container[..MAGIC.len()], MAGIC);
        assert_eq!(container[9..11], FORMAT_VERSION.to_be_bytes());
        assert_eq!(container[11..13], CRYPTO_VERSION.to_be_bytes());
        assert_eq!(container[13], KDF_ARGON2ID);
        assert_eq!(container[14..18], (8 * 1024u32).to_be_bytes());
        assert_eq!(container[18..22], 1u32.to_be_bytes());
        assert_eq!(container[22..26], 1u32.to_be_bytes());
        // salt at 26..42, nonce at 42..66, and the body repeats the nonce.
        assert_eq!(container[66..66 + NONCE_LEN], container[42..66]);
    }

    #[test]
    fn a_wrong_credential_and_a_tampered_file_fail_identically() {
        let (_root, file) = sample();
        let container = seal_fast("right credential", &file);
        let wrong = open_container("wrong credential", &container).unwrap_err();
        assert!(matches!(wrong, AccountError::OpenFailed));

        let mut tampered = container.clone();
        tampered[HEADER_LEN + 5] ^= 1;
        let edited = open_container("right credential", &tampered).unwrap_err();
        assert_eq!(
            wrong, edited,
            "the two failures an attacker would grind against must look the same"
        );

        // And so does flipping a header byte that stays inside the accepted
        // parameter range: the header is associated data, so this is the tag
        // failing, not the validator. (A byte that lands out of range is
        // refused even earlier, as KdfOutOfRange.)
        let mut header_edit = container;
        header_edit[17] ^= 1;
        assert!(matches!(
            open_container("right credential", &header_edit),
            Err(AccountError::OpenFailed)
        ));
    }

    #[test]
    fn a_file_that_is_not_a_container_is_said_so() {
        let (_root, file) = sample();
        let container = seal_fast("credential", &file);
        assert!(matches!(
            open_container("credential", &container[..HEADER_LEN - 1]),
            Err(AccountError::NotAContainer)
        ));
        let mut wrong_magic = container;
        wrong_magic[0] = b'X';
        assert!(matches!(
            open_container("credential", &wrong_magic),
            Err(AccountError::NotAContainer)
        ));
    }

    #[test]
    fn future_versions_and_unknown_kdfs_are_named_errors() {
        let (_root, file) = sample();
        let mut container = seal_fast("credential", &file);
        container[9..11].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(
            open_container("credential", &container),
            Err(AccountError::UnsupportedVersion {
                found: 2,
                supported: 1
            })
        );
        let mut container = seal_fast("credential", &file);
        container[13] = 9;
        assert_eq!(
            open_container("credential", &container),
            Err(AccountError::UnknownKdf { found: 9 })
        );
    }

    #[test]
    fn hostile_parameters_are_refused_before_the_allocation() {
        let (_root, file) = sample();
        let hostile = ContainerParams {
            memory_kib: 8 * 1024 * 1024 + 1,
            time_cost: 3,
            lanes: 1,
        };
        assert_eq!(
            seal_container_with(
                "credential",
                &file,
                hostile,
                &[0u8; SALT_LEN],
                &[0u8; NONCE_LEN]
            ),
            Err(AccountError::KdfOutOfRange)
        );
    }

    #[test]
    fn credential_length_is_enforced_both_ways() {
        let (_root, file) = sample();
        assert!(matches!(
            seal_container_with(
                "short",
                &file,
                fast_params(),
                &[0u8; SALT_LEN],
                &[0u8; NONCE_LEN]
            ),
            Err(AccountError::BadLength { .. })
        ));
        let long = "x".repeat(MAX_CREDENTIAL_BYTES + 1);
        assert!(matches!(
            seal_container_with(
                &long,
                &file,
                fast_params(),
                &[0u8; SALT_LEN],
                &[0u8; NONCE_LEN]
            ),
            Err(AccountError::BadLength { .. })
        ));
    }

    #[test]
    fn two_sealings_of_the_same_account_differ_and_both_open() {
        // Random salt and nonce: a container copied to two clouds cannot be
        // correlated by bytes, and an attacker cannot learn anything from
        // comparing two backups of the same account.
        let (root, file) = sample();
        let mut random = OsRandom;
        let a = seal_container("credential", &file, &mut random).expect("seal");
        let b = seal_container("credential", &file, &mut random).expect("seal");
        assert_ne!(a, b);
        assert_eq!(
            open_container("credential", &a)
                .and_then(|f| f.root())
                .expect("opens"),
            root
        );
        assert_eq!(
            open_container("credential", &b)
                .and_then(|f| f.root())
                .expect("opens"),
            root
        );
    }
}
