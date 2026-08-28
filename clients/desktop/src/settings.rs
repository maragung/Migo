//! The user's non-secret preferences, including the chosen server endpoint.
//!
//! Sits next to [`crate::vault`] and is its opposite in every respect. The vault holds private keys
//! under a passphrase-derived key and refuses to be read without it; this holds a server endpoint
//! in a plain file that the user expects to be readable. Nothing here is a credential, and the
//! separation is what keeps that true: a settings file that also held the refresh token would have
//! to be sealed, and then a person could not change their server before unlocking their keys.
//!
//! The endpoint is the only field today; the file is JSON so a future field lands without a
//! bespoke codec. The path lives under the platform data directory (XDG on Linux, the same path
//! `directories::ProjectDataDir` would return) so it survives a vault rotation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::config::{default_loopback_server_endpoint, ServerEndpoint};

const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_VERSION: u32 = 1;

/// The on-disk settings record. Versioned so a future field that the older binary does not know
/// about is at least an explicit migration rather than a silent misread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The schema version. A reader that does not recognise the value refuses to load.
    pub version: u32,
    /// The user-configured server.
    pub server: ServerEndpoint,
}

impl Settings {
    /// The default: the loopback dev policy on `localhost:18080`, with the gateway on the next
    /// port. A user who has never opened the server field gets exactly the same defaults the
    /// web client offers on its first visit.
    #[must_use]
    pub fn default_for_dev() -> Self {
        Self {
            version: SETTINGS_VERSION,
            server: default_loopback_server_endpoint("localhost", 18080),
        }
    }

    /// The path to the settings file, under the platform's data directory.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("io", "migo", "migo-desktop")
            .map(|dirs| dirs.data_dir().join(SETTINGS_FILE))
    }
}

/// What a settings read or write can fail with. Each variant maps to a single failure shape, so a
/// caller can pick the one that should not be a user-visible error versus the one that should.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The file is not where it was expected. The most common cause is a first launch.
    #[error("settings file not found")]
    NotFound,
    /// The file is unreadable. Treated as a fatal error; the user can copy the file out and reset.
    #[error("cannot read settings: {0}")]
    Io(#[from] io::Error),
    /// The file is present but not parseable. A corrupt or incompatible record.
    #[error("cannot parse settings: {0}")]
    Parse(#[from] serde_json::Error),
    /// The record is parseable but reports a future version this build does not understand.
    #[error("unsupported settings version: {0}")]
    Version(u32),
}

/// Loads the settings record, or returns the default when no file exists yet.
pub fn load_or_default(path: &Path) -> Settings {
    match load(path) {
        Ok(settings) => settings,
        Err(SettingsError::NotFound) => Settings::default_for_dev(),
        Err(error) => {
            // A parse or version error is a real problem; the safest fallback is the default
            // rather than a panic that loses the user's only working build. The trace goes
            // through the standard logging path so an operator can diagnose without seeing it.
            tracing::warn!("migo-desktop: settings unreadable, using default: {error}");
            Settings::default_for_dev()
        }
    }
}

/// Reads the record, propagating every error including the not-found case.
pub fn load(path: &Path) -> Result<Settings, SettingsError> {
    let text = fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SettingsError::NotFound,
        _ => SettingsError::Io(error),
    })?;
    let settings: Settings = serde_json::from_str(&text)?;
    if settings.version != SETTINGS_VERSION {
        return Err(SettingsError::Version(settings.version));
    }
    Ok(settings)
}

/// Persists the record. The write is a single rename of a sibling temp file, so a half-written
/// file can never be observed as the live one.
///
/// Reserved for the desktop settings UI; not yet wired in this build. The flag is
/// here so the public API does not get pruned.
#[allow(dead_code)]
pub fn save(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(settings)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
