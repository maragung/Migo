//! Migo desktop client.
//!
//! A native end-to-end encrypted messaging client. Private keys are generated here, sealed here, and
//! never sent to a server; the server relays opaque envelopes it cannot read. See the workspace README
//! and specification section 11 for the envelope format.
//!
//! # Layout of the program
//!
//! `theme` and `ui` draw. `model` holds the plain structs the UI reads. `net` owns everything
//! asynchronous on its own thread. `crypto` and `vault` own key material. `app` is the seam: it drains
//! events from `net` into `model` values and hands `ui` a read-only view plus a command buffer.
//!
//! Nothing in `ui` can reach a socket, a ratchet or the vault, because nothing in `ui` is given one.
//! That is deliberate and worth preserving: it is what makes it structurally impossible for a layout
//! function to log a plaintext or block the paint loop on a network call.

// The window is the product; a console behind it on Windows is not.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod config;
mod crypto;
mod model;
mod net;
mod settings;
mod theme;
mod ui;
mod vault;

use std::path::PathBuf;

use crate::config::{default_loopback_server_endpoint, ServerEndpoint};
use crate::settings::{load_or_default, Settings, SettingsError};

/// The fallback server when the env var is unset and no settings file is reachable.
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 18080;

fn main() -> eframe::Result<()> {
    install_tracing();

    let vault_path = match resolve_vault_path() {
        Ok(path) => path,
        Err(error) => {
            // Without somewhere to put the vault there is nothing this program can safely do: keys
            // that cannot be persisted would have to be regenerated on every launch, and every peer's
            // safety number would change every time. Fail loudly rather than run in a broken mode.
            eprintln!("migo-desktop: cannot determine where to store keys: {error}");
            std::process::exit(2);
        }
    };
    let server = resolve_server_endpoint();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Migo")
            .with_inner_size([1080.0, 720.0])
            // Below this the sidebar and the thread cannot both be usable, so the window refuses to
            // become a shape the layout cannot honour rather than degrading into overlap.
            .with_min_inner_size([720.0, 480.0])
            .with_app_id("io.migo.desktop"),
        // Everything else is left at eframe's defaults on purpose. A chat window needs no
        // multisampling, no depth or stencil buffer, and no persisted window geometry beyond what
        // eframe already does; naming those fields here would only pin values that upstream is
        // better placed to choose.
        ..Default::default()
    };

    eframe::run_native(
        "Migo",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, vault_path, server)))),
    )
}

/// What server to offer on first run.
///
/// `MIGO_SERVER` overrides the persisted endpoint (so a CI or test harness can point at a
/// different node without rewriting the settings file), and the settings file is the user's last
/// typed choice. A first launch with nothing anywhere falls back to the loopback dev policy.
fn resolve_server_endpoint() -> ServerEndpoint {
    if let Ok(raw) = std::env::var("MIGO_SERVER") {
        if let Some(endpoint) = parse_env_server(&raw) {
            return endpoint;
        }
    }
    if let Some(path) = Settings::default_path() {
        let settings = load_or_default(&path);
        return settings.server;
    }
    default_loopback_server_endpoint(DEFAULT_HOST, DEFAULT_PORT)
}

/// Parses the env-supplied URL into a {@link ServerEndpoint}. A malformed URL falls back to the
/// default rather than refusing to start: the env is a developer convenience, not a contract.
fn parse_env_server(raw: &str) -> Option<ServerEndpoint> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = url_simple_parse(trimmed)?;
    let rest_scheme = match url.scheme.as_str() {
        "https" => crate::config::RestScheme::Https,
        "http" => crate::config::RestScheme::Http,
        _ => return None,
    };
    let host = url.host;
    let port = url
        .port
        .unwrap_or(if rest_scheme == crate::config::RestScheme::Https {
            443
        } else {
            80
        });
    let scheme = if rest_scheme == crate::config::RestScheme::Https {
        crate::config::Scheme::Ws(crate::config::WsScheme::Wss)
    } else {
        crate::config::Scheme::Ws(crate::config::WsScheme::Ws)
    };
    Some(ServerEndpoint {
        host,
        port,
        gateway_port: port.saturating_add(1),
        transport: crate::config::Transport::WebSocket,
        scheme,
        rest_scheme,
    })
}

struct SimpleUrl {
    scheme: String,
    host: String,
    port: Option<u16>,
}

fn url_simple_parse(input: &str) -> Option<SimpleUrl> {
    let (scheme, rest) = input.split_once("://")?;
    let (authority, _) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) => {
            if host.is_empty() {
                return None;
            }
            // The authority is `host:port`; the port must be digits.
            if port_text.chars().all(|c| c.is_ascii_digit()) && !port_text.is_empty() {
                let port = port_text.parse::<u16>().ok()?;
                if port == 0 {
                    return None;
                }
                (host.to_ascii_lowercase(), Some(port))
            } else {
                (authority.to_ascii_lowercase(), None)
            }
        }
        None => (authority.to_ascii_lowercase(), None),
    };
    Some(SimpleUrl {
        scheme: scheme.to_ascii_lowercase(),
        host,
        port,
    })
}

#[allow(dead_code)]
fn _settings_error_marker(_: SettingsError) {}

/// Where the vault lives.
///
/// `MIGO_VAULT` overrides it, which is what makes it possible to run two accounts side by side for
/// testing without either one clobbering the other's keys.
fn resolve_vault_path() -> Result<PathBuf, vault::VaultError> {
    match std::env::var("MIGO_VAULT") {
        Ok(path) if !path.trim().is_empty() => Ok(PathBuf::from(path)),
        _ => vault::default_path(),
    }
}

/// Logging, off unless asked for.
///
/// `RUST_LOG` is honoured but nothing is logged by default. Specification section 174 forbids
/// plaintext, envelopes, key material and raw tokens in logs; the surest way to honour that on a
/// client is to keep the default quiet and to have written no call site that could emit one.
fn install_tracing() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .try_init();
}
