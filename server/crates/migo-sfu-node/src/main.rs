//! The `migosfud` binary: the group-call media plane, on a socket of its own.
//!
//! Section 92 puts the SFU outside `migod` because its load profile is bandwidth rather than
//! application logic, and section 166 requires that it only forwards sealed frames. This process
//! is that separation made real: it holds one HMAC key to admit devices and no call key at all,
//! it opens two sockets — the media listener and, when configured, a metrics listener — and it
//! serves no MWP connection and no opcode.
//!
//! # Why the configuration is loaded without validation
//!
//! `Config::load` validates every section, and this process would be refused by one it will never
//! open: a `store.url` that is absent because the database belongs to the signalling node, an
//! `http.bind` that is unset for the same reason. So this binary loads the configuration
//! unvalidated and validates the one section it runs — `sfu` — which is the section the operator
//! wrote for *this* deployment. A media process that refuses to start over a section it does not
//! use is one an operator cannot run beside the node it serves; a media process that starts with a
//! broken `sfu` section is one that fails at the first call instead of at startup, which is worse.
//!
//! # Shutdown
//!
//! `SIGTERM` or `SIGINT` stops both listeners from accepting, closes the media endpoint so every
//! session's transport ends, and exits. A media process has nothing to drain the way the gateway
//! does — there is no session state a client must be told to move — so the close is the whole of
//! it: a call whose media process restarts reconnects to the next one with a ticket it can mint
//! again.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use migo_core::metrics::Registry;
use migo_core::{Config, Shutdown};

use migo_sfu_node::Server;

fn main() -> anyhow::Result<()> {
    let config = load_config()?;
    migo_core::telemetry::init(&config.telemetry.log_level, config.telemetry.log_format)
        .map_err(|error| anyhow::anyhow!("cannot install logging: {error}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(serve(config))
}

/// Loads the configuration and validates only the section this process runs.
fn load_config() -> anyhow::Result<Config> {
    let config = Config::load_unvalidated().context("cannot load configuration")?;
    let mut problems = Vec::new();
    config.sfu.validate(&mut problems);
    if !problems.is_empty() {
        let mut message = String::from("sfu configuration is not usable:");
        for problem in problems {
            message.push_str("\n  - ");
            message.push_str(&problem);
        }
        anyhow::bail!(message);
    }
    Ok(config)
}

/// Binds the media plane and its metrics endpoint, then serves until a signal arrives.
async fn serve(config: Config) -> anyhow::Result<()> {
    let shutdown = Shutdown::new();
    shutdown.install_signal_handler();

    let registry = Arc::new(Registry::new());
    let heartbeat = Duration::from_millis(config.gateway.heartbeat_ms);
    let server = Arc::new(
        Server::new(&config.sfu, heartbeat, &registry).context("cannot build the media plane")?,
    );
    let bound = server
        .bind(shutdown.clone())
        .await
        .context("cannot bind the media plane")?;

    let metrics = match config.sfu.metrics_bind.as_deref() {
        Some(bind) => {
            let bound = migo_sfu_node::serve_metrics(bind, Arc::clone(&registry), shutdown.clone())
                .await
                .with_context(|| format!("cannot bind the metrics listener to {bind}"))?;
            Some(bound)
        }
        None => None,
    };

    tracing::info!(
        media = %bound,
        metrics = ?metrics,
        public_url = %config.sfu.public_url,
        queue = server.plane().queue_capacity(),
        keep_alive_ms = heartbeat.as_millis() / 2,
        "the media plane is serving"
    );

    shutdown.cancelled().await;
    tracing::info!("the media plane is shutting down");
    Ok(())
}
