//! migod: the Migo server daemon, and the composition root that wires every layer into it.
//!
//! The daemon is a thin binary over this library. Everything that makes a server — building the
//! twenty-one crates into one connected system ([`App`]), the dispatcher that routes opcodes into
//! the domain ([`dispatch`]), the deployment ports the domain leaves open ([`ports`]) — lives here,
//! so the integration harness can build an [`App`] against in-memory backends and drive it without
//! ever opening a socket. `main` loads the configuration, initialises logging from it, and
//! calls [`run_blocking`].
//!
//! # Layering
//!
//! This is the top of the stack (layer 5) and the one crate that may depend on every other. It
//! reaches down into all four layers below and connects them; nothing depends back up on it. The
//! composition root is the single place allowed to know the whole graph, which is precisely why
//! every other crate can stay ignorant of it.

pub mod call_sweep;
pub mod cli;
pub mod dispatch;
pub mod message_sweep;
pub mod ports;
pub mod room_presence;

mod compose;
pub mod mesh;
pub mod quic;
mod serve;
pub mod tcp;
mod transport;
mod webhook;

pub use compose::App;
pub use serve::GATEWAY_PATH;

use anyhow::Context;

use migo_core::Config;

/// Serves the configuration the caller already loaded until shutdown. Blocks the caller.
///
/// This is the whole of what the binary does after startup: it builds a multi-threaded
/// runtime, takes the [`Config`] that `main` loaded (and initialised logging from — see
/// `main.rs` for why the load happens there rather than here), constructs the [`App`],
/// installs the signal handler that turns SIGTERM and SIGINT into a graceful shutdown, and
/// serves until that shutdown completes. The signal handler is installed from inside the
/// runtime because it spawns a task to watch for the signal.
///
/// # Errors
///
/// Returns an error if the runtime cannot be built, the [`App`] cannot be constructed, or
/// the server terminates abnormally.
pub fn run_blocking(config: Config) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot build the async runtime")?;

    runtime.block_on(async {
        let app = App::build(&config).await?;
        app.shutdown.install_signal_handler();
        app.serve().await
    })
}
