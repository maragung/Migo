//! The `migod` binary: parse the command line, then either print and exit or hand off to serve.
//!
//! Everything of substance is in the `migod` library ([`migod::run_blocking`]); this entry point
//! answers the two questions that must not start a server — `--help` and `--version` — and, for the
//! default serve command, installs a tracing subscriber before handing off. Choosing how logs are
//! formatted and filtered is a decision for the process, not for the library a test also links, and
//! it happens only on the path that actually serves: `migod --version` touches no logging, no
//! configuration, and no socket.

use migod::cli::{self, Command, EXIT_USAGE};

/// Parses arguments, then prints-and-exits or serves.
fn main() -> anyhow::Result<()> {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Serve) => {
            init_tracing();
            migod::run_blocking()
        }
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
