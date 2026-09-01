//! The on-disk vault: this device's private keys, encrypted under the user's passphrase.
//!
//! # Why a passphrase and not the OS keyring
//!
//! The Android client puts its keys in the hardware-backed Keystore and the web client in IndexedDB
//! behind the browser's origin isolation. A Linux desktop has no equivalent that is present
//! everywhere: Secret Service needs a running D-Bus session and an unlocked login keyring, and a file
//! sitting readable in `~/.config` protects nothing at all. A passphrase-derived key works on a
//! server over SSH, on a live USB and on a laptop with no desktop environment, and it is the same
//! guarantee in each case. Adding keyring support later is a second [`Vault`] backend, not a change
//! to anything above this module.
//!
//! # The format
//!
//! ```text
//! "MIGOVLT1"  8 bytes magic
//! u8          format version
//! 16 bytes    Argon2id salt
//! u32 LE      memory cost, KiB
//! u32 LE      time cost, passes
//! u32 LE      lanes
//! remainder   XChaCha20-Poly1305 sealed body: nonce || ciphertext || tag
//! ```
//!
//! The Argon2 parameters are stored rather than compiled in, so raising the cost for new vaults does
//! not lock anyone out of an old one. The whole header — magic, version, salt and all three
//! parameters — is the AEAD's associated data, which is what stops an attacker from lowering the
//! stored cost to something brute-forcible: the tag fails before the weakened parameters are ever
//! used. The body decodes to the identity seeds, the signed prekey, every unused one-time prekey, and
//! optionally the saved sign-in (server URL, ids, refresh token) so the passphrase alone gets the user
//! back to their conversations.
//!
//! The refresh token is bearer material, and this is the right place for it: the alternative is a
//! plaintext line in a config file, which is worse in every way. Anything after the prekey list is an
//! MSE optional field, so a vault written by a build that saves more can still be read by one that
//! does not.
//!
//! # Cost
//!
//! 64 MiB, 3 passes. Well above the server's login parameters (19 MiB, 2 passes) and deliberately so:
//! the server's figure is bounded by what a login spike can allocate at once, while this is one hash
//! on one machine at unlock time. A few hundred milliseconds is imperceptible to the person typing
//! and multiplies an offline attacker's cost by the same factor.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use migo_core::{Id, OsRandom};
use migo_crypto::identity::{KeyPair, PUBLIC_KEY_LEN};
use migo_crypto::{IdentitySecret, SymmetricKey};
use migo_wire::{Reader, Writer};
use zeroize::Zeroize;

use crate::crypto::session::DeviceKeys;

/// The optional-field id under which the saved sign-in lives in the vault body.
const FIELD_SESSION: u32 = 1;

/// The optional-field id under which the unified account root lives.
///
/// Stored as the raw 32 bytes, sealed with everything else, and present only on a device that
/// founded the account or restored a `.migo` container onto it.
const FIELD_ROOT: u32 = 2;

/// The optional-field id under which the ML-DSA device credential seed lives.
const FIELD_DEVICE_CREDENTIAL: u32 = 3;

/// A saved sign-in, so launching the client is one passphrase rather than a full login.
///
/// The access token is deliberately absent: it lives for minutes, so persisting it would buy nothing
/// and widen the window in which a stolen vault is directly usable. The refresh token is exchanged
/// for a fresh pair on every start, and the server rotates it — a replay of an old one is detected
/// there as refresh reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    /// Base URL of the server this account belongs to, e.g. `https://migo.example`.
    pub server_url: String,
    /// The account.
    pub account_id: Id,
    /// This device, as registered with the server. Stable across sign-ins.
    pub device_id: Id,
    /// The username, so the unlock screen can greet the right person before any network call.
    pub username: String,
    /// The refresh token, exchanged for an access token at startup.
    pub refresh_token: String,
}

const MAGIC: &[u8; 8] = b"MIGOVLT1";
const FORMAT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN + 4 * 3;

/// Argon2id memory cost for new vaults, in KiB.
pub const MEMORY_KIB: u32 = 64 * 1024;
/// Argon2id passes for new vaults.
pub const TIME_COST: u32 = 3;
/// Argon2id lanes for new vaults.
pub const LANES: u32 = 1;

/// Shortest passphrase accepted.
///
/// Length is the only rule. Composition requirements measurably push people towards `Passw0rd!`,
/// which is in every cracking dictionary; a long phrase they can actually remember is stronger.
pub const MIN_PASSPHRASE_BYTES: usize = 8;

/// Longest passphrase accepted, so a pasted file cannot turn one unlock into a minute of hashing.
pub const MAX_PASSPHRASE_BYTES: usize = 1024;

/// What can go wrong opening or saving a vault.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault file: {0}")]
    Io(#[from] io::Error),

    #[error("this file is not a Migo vault")]
    NotAVault,

    #[error("vault was written by a newer version of this client")]
    FutureVersion,

    /// The passphrase was wrong, or the file was edited. Which one is not distinguished: the AEAD
    /// cannot tell, and a message that guessed would be a hint an attacker could grind against.
    #[error("wrong passphrase, or the vault file has been modified")]
    Locked,

    #[error("vault contents are malformed")]
    Malformed,

    #[error("passphrase must be between {MIN_PASSPHRASE_BYTES} and {MAX_PASSPHRASE_BYTES} bytes")]
    PassphraseLength,

    #[error("could not determine a configuration directory for this user")]
    NoConfigDir,
}

/// The stored Argon2 parameters, read from a vault header or chosen for a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cost {
    memory_kib: u32,
    time_cost: u32,
    lanes: u32,
}

impl Cost {
    const CURRENT: Self = Self {
        memory_kib: MEMORY_KIB,
        time_cost: TIME_COST,
        lanes: LANES,
    };

    /// Rejects parameters that would make unlocking free.
    ///
    /// A stored cost is attacker-controlled input in the sense that anyone who can write the file can
    /// set it. The tag over the header already stops a silent downgrade, but a floor here means a
    /// malformed file fails fast rather than after a pointless hash, and it documents the minimum
    /// this client will ever accept.
    fn validate(self) -> Result<Self, VaultError> {
        let sane = self.memory_kib >= 8 * 1024
            && self.memory_kib <= 4 * 1024 * 1024
            && (1..=16).contains(&self.time_cost)
            && (1..=16).contains(&self.lanes);
        if sane {
            Ok(self)
        } else {
            Err(VaultError::Malformed)
        }
    }
}

/// Derives the vault key from a passphrase.
fn derive_key(passphrase: &str, salt: &[u8], cost: Cost) -> Result<SymmetricKey, VaultError> {
    let params = Params::new(cost.memory_kib, cost.time_cost, cost.lanes, Some(32))
        .map_err(|_| VaultError::Malformed)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| VaultError::Malformed)?;
    let wrapped = SymmetricKey::from_bytes(key);
    key.zeroize();
    Ok(wrapped)
}

/// Where the vault lives, by convention for the platform.
///
/// `$XDG_CONFIG_HOME/migo/vault.bin` on Linux, `~/Library/Application Support/…` on macOS,
/// `%APPDATA%\…` on Windows — whatever [`directories`] reports, so the file lands where the platform
/// expects private per-user configuration rather than where this program finds convenient.
pub fn default_path() -> Result<PathBuf, VaultError> {
    let dirs =
        directories::ProjectDirs::from("io", "Migo", "Migo").ok_or(VaultError::NoConfigDir)?;
    Ok(dirs.config_dir().join("vault.bin"))
}

/// Whether a vault already exists at `path`. Decides between the unlock and the create screen.
#[must_use]
pub fn exists(path: &Path) -> bool {
    path.is_file()
}

/// Reads and decrypts a vault.
pub fn load(path: &Path, passphrase: &str) -> Result<DeviceKeys, VaultError> {
    check_passphrase(passphrase)?;
    let raw = fs::read(path)?;
    if raw.len() < HEADER_LEN {
        return Err(VaultError::NotAVault);
    }
    let (header, body) = raw.split_at(HEADER_LEN);
    if &header[..MAGIC.len()] != MAGIC {
        return Err(VaultError::NotAVault);
    }
    let version = header[MAGIC.len()];
    if version != FORMAT_VERSION {
        // A newer client may have written a format this one cannot read. Refusing beats truncating
        // someone's only copy of their identity key.
        return Err(VaultError::FutureVersion);
    }
    let salt = &header[MAGIC.len() + 1..MAGIC.len() + 1 + SALT_LEN];
    let numbers = &header[MAGIC.len() + 1 + SALT_LEN..];
    let cost = Cost {
        memory_kib: u32::from_le_bytes(numbers[0..4].try_into().expect("4 bytes")),
        time_cost: u32::from_le_bytes(numbers[4..8].try_into().expect("4 bytes")),
        lanes: u32::from_le_bytes(numbers[8..12].try_into().expect("4 bytes")),
    }
    .validate()?;

    let key = derive_key(passphrase, salt, cost)?;
    // The whole header is the associated data, so editing the salt or lowering the cost breaks the
    // tag instead of weakening the vault.
    let plaintext = migo_crypto::open(&key, header, body).map_err(|_| VaultError::Locked)?;
    let keys = decode_keys(&plaintext);
    // The decoded material now lives in `DeviceKeys`; the serialised copy must not linger in a heap
    // buffer that will be reused for something else.
    let mut plaintext = plaintext;
    plaintext.zeroize();
    keys
}

/// Encrypts and writes a vault, replacing any existing file.
///
/// The write is atomic: a temporary file in the same directory, then a rename. A crash halfway
/// through an in-place write would leave a truncated vault, and a truncated vault is an identity that
/// cannot be recovered — every message ever sent to this device becomes unreadable. A rename either
/// happened or it did not.
pub fn save(path: &Path, passphrase: &str, keys: &DeviceKeys) -> Result<(), VaultError> {
    check_passphrase(passphrase)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut salt = [0u8; SALT_LEN];
    {
        use migo_core::Random;
        OsRandom.fill_bytes(&mut salt);
    }
    let cost = Cost::CURRENT;

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.push(FORMAT_VERSION);
    header.extend_from_slice(&salt);
    header.extend_from_slice(&cost.memory_kib.to_le_bytes());
    header.extend_from_slice(&cost.time_cost.to_le_bytes());
    header.extend_from_slice(&cost.lanes.to_le_bytes());
    debug_assert_eq!(header.len(), HEADER_LEN);

    let key = derive_key(passphrase, &salt, cost)?;
    let mut plaintext = encode_keys(keys)?;
    let mut random = OsRandom;
    let body = migo_crypto::seal(&key, &header, &plaintext, &mut random)
        .map_err(|_| VaultError::Malformed)?;
    plaintext.zeroize();

    let mut out = header;
    out.extend_from_slice(&body);

    let temporary = path.with_extension("bin.new");
    fs::write(&temporary, &out)?;
    restrict(&temporary)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Makes the file readable only by its owner.
///
/// The vault is encrypted, so a stolen copy is not immediately readable — but an offline attacker
/// only has to guess a passphrase, and there is no reason to hand another local account the
/// ciphertext to grind against. On Unix this is `0o600`; elsewhere the platform default applies,
/// which is why the vault is encrypted rather than relying on permissions.
fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn check_passphrase(passphrase: &str) -> Result<(), VaultError> {
    let length = passphrase.len();
    if (MIN_PASSPHRASE_BYTES..=MAX_PASSPHRASE_BYTES).contains(&length) {
        Ok(())
    } else {
        Err(VaultError::PassphraseLength)
    }
}

/// Serialises the private key material with the same wire codec the protocol uses.
fn encode_keys(keys: &DeviceKeys) -> Result<Vec<u8>, VaultError> {
    let mut w = Writer::new();
    w.enter().map_err(|_| VaultError::Malformed)?;
    w.write_bytes(&keys.identity.expose_signing_seed())
        .map_err(|_| VaultError::Malformed)?;
    w.write_bytes(&keys.identity.expose_exchange_seed())
        .map_err(|_| VaultError::Malformed)?;
    w.write_u32(keys.signed_prekey_id);
    w.write_bytes(&keys.signed_prekey.expose_seed())
        .map_err(|_| VaultError::Malformed)?;

    // Sorted so saving the same vault twice produces the same plaintext, which keeps a diff of two
    // backups meaningful and makes the format's tests deterministic.
    let mut one_time: Vec<(&u32, &KeyPair)> = keys.one_time.iter().collect();
    one_time.sort_unstable_by_key(|(id, _)| **id);
    w.list_len(one_time.len())
        .map_err(|_| VaultError::Malformed)?;
    for (id, pair) in one_time {
        w.write_u32(*id);
        w.write_bytes(&pair.expose_seed())
            .map_err(|_| VaultError::Malformed)?;
    }

    let optionals = u32::from(keys.session.is_some())
        + u32::from(keys.root.is_some())
        + u32::from(keys.device_credential_seed.is_some());
    w.write_u32(optionals);
    if let Some(saved) = &keys.session {
        w.optional(FIELD_SESSION, |w| {
            w.write_str(&saved.server_url)?;
            w.write_id(&saved.account_id);
            w.write_id(&saved.device_id);
            w.write_str(&saved.username)?;
            w.write_str(&saved.refresh_token)
        })
        .map_err(|_| VaultError::Malformed)?;
    }
    if let Some(root) = &keys.root {
        w.optional(FIELD_ROOT, |w| w.write_bytes(root))
            .map_err(|_| VaultError::Malformed)?;
    }
    if let Some(seed) = &keys.device_credential_seed {
        w.optional(FIELD_DEVICE_CREDENTIAL, |w| w.write_bytes(seed))
            .map_err(|_| VaultError::Malformed)?;
    }
    w.leave();
    w.finish_vec().map_err(|_| VaultError::Malformed)
}

fn decode_keys(plaintext: &[u8]) -> Result<DeviceKeys, VaultError> {
    let mut r = Reader::from_slice(plaintext);
    r.enter().map_err(|_| VaultError::Malformed)?;
    let signing = seed32(&mut r)?;
    let exchange = seed32(&mut r)?;
    let identity = IdentitySecret::from_seeds(signing, exchange);
    let signed_prekey_id = r.read_u32().map_err(|_| VaultError::Malformed)?;
    let signed_prekey = KeyPair::from_seed(seed32(&mut r)?);
    let count = r.read_list_len().map_err(|_| VaultError::Malformed)?;
    let mut one_time = std::collections::HashMap::with_capacity(count);
    for _ in 0..count {
        let id = r.read_u32().map_err(|_| VaultError::Malformed)?;
        one_time.insert(id, KeyPair::from_seed(seed32(&mut r)?));
    }

    let optionals = r.read_u32().map_err(|_| VaultError::Malformed)?;
    let mut session = None;
    let mut root = None;
    let mut device_credential_seed = None;
    for _ in 0..optionals {
        let (field, mut inner) = r.read_optional().map_err(|_| VaultError::Malformed)?;
        // An unknown id is skipped, not an error: the sub-reader is length-scoped, so a newer build's
        // extra field costs this one nothing and cannot desynchronise the rest of the body.
        if field == FIELD_SESSION {
            session = Some(SavedSession {
                server_url: inner.read_string().map_err(|_| VaultError::Malformed)?,
                account_id: inner.read_id().map_err(|_| VaultError::Malformed)?,
                device_id: inner.read_id().map_err(|_| VaultError::Malformed)?,
                username: inner.read_string().map_err(|_| VaultError::Malformed)?,
                refresh_token: inner.read_string().map_err(|_| VaultError::Malformed)?,
            });
        } else if field == FIELD_ROOT {
            root = Some(seed32(&mut inner)?);
        } else if field == FIELD_DEVICE_CREDENTIAL {
            device_credential_seed = Some(seed32(&mut inner)?);
        }
    }
    r.leave();
    Ok(DeviceKeys {
        identity,
        signed_prekey_id,
        signed_prekey,
        one_time,
        session,
        root,
        device_credential_seed,
    })
}

fn seed32(r: &mut Reader) -> Result<[u8; 32], VaultError> {
    let mut bytes = r.read_bytes().map_err(|_| VaultError::Malformed)?;
    let out: [u8; PUBLIC_KEY_LEN] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Malformed)?;
    bytes.zeroize();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::session::DeviceKeys;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn saved_session() -> SavedSession {
        SavedSession {
            server_url: "https://migo.example".to_owned(),
            account_id: Id::from_bytes([1; 16]),
            device_id: Id::from_bytes([2; 16]),
            username: "whoever".to_owned(),
            refresh_token: "single-use".to_owned(),
        }
    }

    /// A founding device's vault carries the root and the credential, and both have to come back:
    /// the root is the account, and a credential that did not survive its own backup would make
    /// the device unable to take part in its own future logins.
    #[test]
    fn a_founding_device_round_trips_with_its_root_and_credential() {
        let path = scratch("migo-vault-founding-roundtrip.bin");
        let root = migo_account::MigoRoot::from_bytes(&[7u8; 32]).expect("32 bytes is a root");
        let mut keys = DeviceKeys::founding(&root);
        keys.session = Some(saved_session());

        save(&path, "correct horse battery", &keys).expect("saved");
        let opened = load(&path, "correct horse battery").expect("opened");
        let _ = std::fs::remove_file(&path);

        assert_eq!(opened.root, keys.root);
        assert_eq!(opened.device_credential_seed, keys.device_credential_seed);
        assert_eq!(opened.session, keys.session);
        assert_eq!(
            opened.identity.expose_signing_seed(),
            keys.identity.expose_signing_seed()
        );
        assert_eq!(
            opened.identity.expose_exchange_seed(),
            keys.identity.expose_exchange_seed()
        );
        assert_eq!(opened.signed_prekey_id, keys.signed_prekey_id);
    }

    /// A wrong passphrase is refused, and the refusal does not leak which field would have been
    /// inside — the sealed body is one blob and the tag fails for all of it at once.
    #[test]
    fn a_founding_vault_refuses_the_wrong_passphrase() {
        let path = scratch("migo-vault-wrong-passphrase.bin");
        let root = migo_account::MigoRoot::from_bytes(&[8u8; 32]).expect("32 bytes is a root");
        let mut keys = DeviceKeys::founding(&root);
        keys.session = Some(saved_session());

        save(&path, "correct horse battery", &keys).expect("saved");
        let outcome = load(&path, "wrong horse battery");
        let _ = std::fs::remove_file(&path);

        assert!(matches!(outcome, Err(VaultError::Locked)));
    }

    /// A body written before the account-root fields existed — no optionals at all — still opens,
    /// as a device that signed in with a password before the upgrade does.
    #[test]
    fn a_body_from_before_the_root_still_decodes() {
        let identity = migo_crypto::IdentitySecret::generate(&mut OsRandom);
        let prekey = migo_crypto::identity::KeyPair::generate(&mut OsRandom);
        let mut w = Writer::new();
        w.enter().expect("enter");
        w.write_bytes(&identity.expose_signing_seed())
            .expect("write");
        w.write_bytes(&identity.expose_exchange_seed())
            .expect("write");
        w.write_u32(1);
        w.write_bytes(&prekey.expose_seed()).expect("write");
        w.list_len(0).expect("empty list");
        w.write_u32(0); // no optional fields
        w.leave();
        let plaintext = w.finish_vec().expect("body");

        let keys = decode_keys(&plaintext).expect("decoded");
        assert!(keys.root.is_none());
        assert!(keys.device_credential_seed.is_none());
        assert!(keys.session.is_none());
        assert_eq!(keys.signed_prekey_id, 1);
        assert_eq!(
            keys.identity.expose_signing_seed(),
            identity.expose_signing_seed()
        );
    }

    /// A body carrying only the session optional — the exact shape a pre-root build saved on every
    /// successful sign-in — decodes the session and leaves the root fields empty, rather than
    /// misreading the older field id as one of the new ones.
    #[test]
    fn a_session_only_body_decodes_the_session_and_nothing_else() {
        let identity = migo_crypto::IdentitySecret::generate(&mut OsRandom);
        let prekey = migo_crypto::identity::KeyPair::generate(&mut OsRandom);
        let saved = saved_session();
        let mut w = Writer::new();
        w.enter().expect("enter");
        w.write_bytes(&identity.expose_signing_seed())
            .expect("write");
        w.write_bytes(&identity.expose_exchange_seed())
            .expect("write");
        w.write_u32(4);
        w.write_bytes(&prekey.expose_seed()).expect("write");
        w.list_len(0).expect("empty list");
        w.write_u32(1); // one optional: the session
        w.optional(FIELD_SESSION, |w| {
            w.write_str(&saved.server_url)?;
            w.write_id(&saved.account_id);
            w.write_id(&saved.device_id);
            w.write_str(&saved.username)?;
            w.write_str(&saved.refresh_token)
        })
        .expect("session field");
        w.leave();
        let plaintext = w.finish_vec().expect("body");

        let keys = decode_keys(&plaintext).expect("decoded");
        assert_eq!(keys.session, Some(saved));
        assert!(keys.root.is_none());
        assert!(keys.device_credential_seed.is_none());
    }
}
