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
mod crypto;
mod model;
mod net;
mod theme;
mod ui;
mod vault;

use std::path::PathBuf;

/// The server address offered on first run when nothing else says otherwise.
const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

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
    let server = std::env::var("MIGO_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_owned());

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
