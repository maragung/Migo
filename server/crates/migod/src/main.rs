//! The `migod` binary: parse the command line, then either print and exit or hand off to serve.
//!
//! Everything of substance is in the `migod` library ([`migod::run_blocking`]); this entry point
//! answers the two questions that must not start a server — `--help` and `--version` — and, for
//! the default serve command, loads the configuration, installs the tracing subscriber it asks
//! for, and hands both off. Choosing how logs are formatted and filtered is a decision for the
//! process, not for the library a test also links, and it happens only on the path that actually
//! serves: `migod --version` touches no logging, no configuration, and no socket.

use anyhow::Context;
use migo_core::Config;
use migod::cli::{self, Command, EXIT_USAGE};

/// Parses arguments, then prints-and-exits or serves.
fn main() -> anyhow::Result<()> {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Serve) => serve(),
        Ok(Command::Help) => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Ok(Command::Version) => {
            println!("{}", cli::VERSION_LINE);
            Ok(())
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(EXIT_USAGE);
        }
    }
}

/// Loads configuration, installs the logging it describes, and serves.
///
/// The configuration is loaded *here* rather than inside `run_blocking` so that
/// `telemetry.log_level` and `telemetry.log_format` — the whole point of those fields —
/// shape the subscriber before the runtime and the app emit their first lines, and the
/// same value is handed down rather than loaded a second time.
fn serve() -> anyhow::Result<()> {
    let config = Config::load().context("cannot load configuration")?;
    init_tracing(&config)?;
    migod::run_blocking(config)
}

/// Installs the tracing subscriber the configuration asks for, via
/// `migo_core::telemetry` — the one installation path in the workspace, so the
/// binary and any test that initialises logging agree on the output shapes.
///
/// Precedence, highest first: the `RUST_LOG` environment variable, then
/// `telemetry.log_level`, whose default stands in when neither is set. Keeping
/// `RUST_LOG` on top preserves what this binary always did — an operator can
/// raise the level of a running deployment without editing the file — while the
/// file becomes the first place a deployment sets its baseline. `log_format` has
/// no environment override: the file is the only place the pretty/json choice is
/// made, because it is a property of where the logs are *going*, not of the
/// shell that happened to start the process.
///
/// Log lines never carry a raw token: the subsystems that handle access,
/// refresh, bot, and push tokens record only their hashes (brief sections 77 and
/// 145), so what a subscriber renders is safe to keep. A malformed directive or
/// an already-installed subscriber aborts startup rather than degrading to
/// unconfigured logging — a server whose log shape is unknown is quietly
/// unauditable.
fn init_tracing(config: &Config) -> anyhow::Result<()> {
    migo_core::telemetry::init(&config.telemetry.log_level, config.telemetry.log_format)
        .map_err(|error| anyhow::anyhow!("cannot install logging: {error}"))
}
