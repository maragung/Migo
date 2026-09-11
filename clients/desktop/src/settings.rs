//! The user's non-secret preferences, including the chosen server endpoint.
//!
//! Sits next to [`crate::vault`] and is its opposite in every respect. The vault holds private keys
//! under a passphrase-derived key and refuses to be read without it; this holds a server endpoint
//! in a plain file that the user expects to be readable. Nothing here is a credential, and the
//! separation is what keeps that true: a settings file that also held the refresh token would have
//! to be sealed, and then a person could not change their server before unlocking their keys.
//!
//! The endpoint is one of two fields today, the other being the chosen theme; the file is JSON
//! so a future field lands without a bespoke codec. The path lives under the platform data
//! directory (XDG on Linux, the same path `directories::ProjectDataDir` would return) so it
//! survives a vault rotation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::config::{default_production_server_endpoint, rest_base_url, ServerEndpoint};
use crate::theme::Theme;

const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_VERSION: u32 = 1;

/// The on-disk settings record. Versioned so a future field that the older binary does not know
/// about is at least an explicit migration rather than a silent misread.
///
/// Fields added after version 1 are `#[serde(default)]` rather than a version bump: refusing to
/// load every pre-existing file would cost a user their saved server for the sake of a field
/// they have never set, and an older binary simply ignores a field it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The schema version. A reader that does not recognise the value refuses to load.
    pub version: u32,
    /// The user-configured server.
    pub server: ServerEndpoint,
    /// The theme the user last chose with the toggle. Absent — a first run, or a file written
    /// before the field existed — means "follow the desktop's own preference".
    #[serde(default)]
    pub theme: Option<Theme>,
    /// The interface scale, as a whole percentage of the default zoom (85, 100, 115, 130).
    /// Absent means 100. A whole number rather than a float so the record stays `Eq` and the
    /// file stays readable: a person opening settings.json should find `"ui_scale": 130`, not
    /// `1.3000001`.
    #[serde(default)]
    pub ui_scale: Option<u8>,
    /// Whether closing a conversation's window writes its transcript to the device as a
    /// plaintext log. Off by default: an E2EE app that silently wrote decrypted chats to disk
    /// by default would be keeping a copy the encryption never promised, so the copy begins
    /// only when the person asks for it. `#[serde(default)]` — a file written before the
    /// field existed simply predates the feature, and the old behaviour (no logs) is the
    /// default.
    #[serde(default)]
    pub auto_save_chat_logs: bool,
}

impl Settings {
    /// The default: the public deployment at `152.53.102.150` — REST on plain HTTP at :8080 and
    /// the native TCP listener at :18081, TCP-first with the WebSocket fallback riding the REST
    /// port. A first-run install talks to the live server immediately, and a user who edits the
    /// field later is overriding one stable default, not chasing one that moves each build.
    #[must_use]
    pub fn default_for_dev() -> Self {
        Self {
            version: SETTINGS_VERSION,
            server: default_production_server_endpoint(),
            theme: None,
            ui_scale: None,
            auto_save_chat_logs: false,
        }
    }

    /// The record's zoom factor, as egui expects it: the percentage over a hundred, and 1.0
    /// whenever no preference was ever saved.
    #[must_use]
    pub fn zoom(&self) -> f32 {
        f32::from(self.ui_scale.unwrap_or(100)) / 100.0
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
        Ok(settings) => heal_stale_server(settings),
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

/// Reconciles a saved server endpoint with the deployment this build belongs to.
///
/// A settings file written by an earlier build may name the deployment host with the ports or
/// TLS posture of an older layout — the WebSocket pair riding the REST port, say, where this
/// build's endpoint is TCP-first on the native listener's port — and the realtime dial then goes
/// to a socket nothing answers while the sign-in form can only report a generic failure. The
/// rule is deliberately narrow: only a record naming *this deployment's host* is rewritten,
/// because that host is ours and its one true endpoint is known. A record naming any other host
/// is a self-hoster's server and is kept exactly as they typed it.
fn heal_stale_server(settings: Settings) -> Settings {
    let deployment = default_production_server_endpoint();
    if settings.server.host != deployment.host || settings.server == deployment {
        return settings;
    }
    tracing::info!(
        "migo-desktop: settings name this deployment's host with a stale endpoint; adopting {}",
        rest_base_url(&deployment)
    );
    Settings {
        server: deployment,
        ..settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RestScheme, Scheme, Transport, WsScheme};

    /// The theme round-trips through the file, because the whole point of storing it is that
    /// the window starts the way it was left. The wire form is pinned too: a settings file is
    /// user-readable, and `"dark"`/`"light"` is what a person expects to find in it.
    #[test]
    fn theme_round_trips() {
        let path = std::env::temp_dir().join("migo-desktop-test-theme.json");
        let record = Settings {
            version: SETTINGS_VERSION,
            server: Settings::default_for_dev().server,
            theme: Some(Theme::Light),
            ui_scale: None,
            auto_save_chat_logs: false,
        };
        save(&path, &record).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.theme, Some(Theme::Light));
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"light\""));
        let _ = fs::remove_file(&path);
    }

    /// The scale round-trips and resolves to a zoom factor, with the absent field reading as the
    /// untouched default — a first run must not inherit a preference it never saved.
    #[test]
    fn ui_scale_round_trips_and_resolves() {
        let path = std::env::temp_dir().join("migo-desktop-test-scale.json");
        let record = Settings {
            ui_scale: Some(130),
            ..Settings::default_for_dev()
        };
        save(&path, &record).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.ui_scale, Some(130));
        assert_eq!(loaded.zoom(), 1.3);
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"ui_scale\": 130"));
        let _ = fs::remove_file(&path);

        assert_eq!(Settings::default_for_dev().zoom(), 1.0);
    }

    /// The chat-log preference round-trips, and a file written before the field existed reads
    /// as off: an E2EE client that "upgraded" a pre-existing install into writing plaintext
    /// transcripts would be turning the feature on for someone who never asked.
    #[test]
    fn auto_save_chat_logs_round_trips_and_defaults_off() {
        let path = std::env::temp_dir().join("migo-desktop-test-chat-log-toggle.json");
        let record = Settings {
            auto_save_chat_logs: true,
            ..Settings::default_for_dev()
        };
        save(&path, &record).expect("save");
        let loaded = load(&path).expect("load");
        assert!(loaded.auto_save_chat_logs);
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"auto_save_chat_logs\": true"));
        let _ = fs::remove_file(&path);

        assert!(!Settings::default_for_dev().auto_save_chat_logs);

        let old_path = std::env::temp_dir().join("migo-desktop-test-chat-log-old.json");
        let old = serde_json::json!({
            "version": SETTINGS_VERSION,
            "server": {
                "host": "localhost",
                "port": 18080,
                "gateway_port": 18081,
                "transport": "WebSocket",
                "scheme": { "Ws": "Ws" },
                "rest_scheme": "Http",
            },
        });
        fs::write(&old_path, old.to_string()).expect("write");
        let loaded = load(&old_path).expect("load");
        assert!(!loaded.auto_save_chat_logs);
        let _ = fs::remove_file(&old_path);
    }

    /// A settings file written before the theme field existed still loads, with the theme
    /// reading as "no preference": the user's saved server must survive the client learning a
    /// new field.
    #[test]
    fn old_file_without_theme_still_loads() {
        let path = std::env::temp_dir().join("migo-desktop-test-old.json");
        let old = serde_json::json!({
            "version": SETTINGS_VERSION,
            "server": {
                "host": "localhost",
                "port": 18080,
                "gateway_port": 18081,
                "transport": "WebSocket",
                "scheme": { "Ws": "Ws" },
                "rest_scheme": "Http",
            },
        });
        fs::write(&path, old.to_string()).expect("write");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.theme, None);
        assert_eq!(loaded.server.host, "localhost");
        let _ = fs::remove_file(&path);
    }

    /// A record naming this deployment's host with an older layout is healed to the deployment's
    /// endpoint. Today that means the WebSocket record every earlier build wrote (the gateway
    /// riding the REST port) becoming the TCP-first one with the native listener's port: the
    /// host is ours, so its one true address is known. Anything else a user typed is theirs.
    #[test]
    fn stale_deployment_endpoint_is_healed() {
        let stale = Settings {
            version: SETTINGS_VERSION,
            server: ServerEndpoint {
                host: "152.53.102.150".to_owned(),
                port: 8080,
                gateway_port: 8080,
                transport: Transport::WebSocket,
                scheme: Scheme::Ws(WsScheme::Ws),
                rest_scheme: RestScheme::Http,
            },
            theme: Some(Theme::Dark),
            ui_scale: None,
            auto_save_chat_logs: false,
        };
        let healed = heal_stale_server(stale);
        assert_eq!(healed.server, default_production_server_endpoint());
        // And the healed record really is TCP-first: the native listener's port and transport.
        assert_eq!(healed.server.transport, Transport::Tcp);
        assert_eq!(healed.server.gateway_port, 18081);
        // The theme is untouched: the healing is about the address, not the record.
        assert_eq!(healed.theme, Some(Theme::Dark));
    }

    /// A self-hoster's record keeps its hand-typed ports and TLS posture: only this
    /// deployment's own host is ever rewritten.
    #[test]
    fn self_hosted_endpoint_is_kept() {
        let mine = Settings {
            version: SETTINGS_VERSION,
            server: ServerEndpoint {
                host: "home.example.org".to_owned(),
                port: 18080,
                gateway_port: 18081,
                transport: Transport::WebSocket,
                scheme: Scheme::Ws(WsScheme::Ws),
                rest_scheme: RestScheme::Http,
            },
            theme: None,
            ui_scale: None,
            auto_save_chat_logs: false,
        };
        assert_eq!(heal_stale_server(mine.clone()), mine);
    }
}
