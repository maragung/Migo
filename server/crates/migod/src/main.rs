//! The `migod` binary: initialise logging, then hand off to the library.
//!
//! Everything of substance is in the `migod` library ([`migod::run_blocking`]); this entry point
//! exists only to install a tracing subscriber before the server starts, because choosing how logs
//! are formatted and filtered is a decision for the process, not for the library a test also links.

/// Installs logging, then runs the server until shutdown.
fn main() -> anyhow::Result<()> {
    init_tracing();
    migod::run_blocking()
}

/// Installs a tracing subscriber filtered by the `RUST_LOG` environment variable.
///
/// Defaults to the `info` level when `RUST_LOG` is unset. Log lines never carry a raw token: the
/// subsystems that handle access, refresh, bot, and push tokens record only their hashes (brief
/// sections 77 and 145), so what a subscriber renders is safe to keep.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
