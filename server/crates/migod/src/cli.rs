//! The daemon's command line: the tiny surface `main` parses before it does anything else.
//!
//! `migod` is a daemon, not a toolbox. Its whole behaviour is decided by configuration — the
//! `MIGO_*` environment and the TOML files [`Config::load`](migo_core::Config::load) reads — so the
//! command line carries no configuration flags at all. What it does carry is the two requests every
//! well-behaved program answers *without* starting up: print your help, and print your version.
//! Both must return before a socket is bound, a config file is opened, or a database is touched, so
//! that `migod --version` in a Dockerfile or a health probe is instant and side-effect free.
//!
//! Parsing is deliberately kept out of `main` and free of any dependency on a runtime or a config,
//! so it can be exercised directly by a test: [`parse`] is a pure function from arguments to a
//! [`Command`] or a [`UsageError`], and nothing it does can start the server by accident.

/// What the arguments asked the process to do.
///
/// [`Serve`](Command::Serve) is the default and the only mode that builds a runtime and reads
/// configuration; the other two print and exit, touching nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Run the server until shutdown. The default when no recognised flag is given.
    Serve,
    /// Print the help text and exit successfully.
    Help,
    /// Print the version line and exit successfully.
    Version,
}

/// An argument the daemon does not accept.
///
/// Its [`Display`](std::fmt::Display) is the message `main` prints to stderr before exiting with a
/// non-zero status: it names the offending argument and points at `--help`, and it never echoes
/// anything but the argument the caller themselves typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    arg: String,
}

impl UsageError {
    /// The argument that was not recognised.
    #[must_use]
    pub fn arg(&self) -> &str {
        &self.arg
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unrecognized argument '{}'\nUsage: migod [--help] [--version]\n\
             Try 'migod --help' for more information.",
            self.arg
        )
    }
}

impl std::error::Error for UsageError {}

/// The process exit status for a usage error: a misused command line, by long convention,
/// exits `2` — distinct from `1`, which the serve path returns when the server itself fails.
pub const EXIT_USAGE: i32 = 2;

/// The version line printed by `migod --version`, resolved from the crate version at compile time.
pub const VERSION_LINE: &str = concat!("migod ", env!("CARGO_PKG_VERSION"));

/// The full help text printed by `migod --help`.
pub const HELP: &str = "\
migod \u{2014} the Migo server daemon

Usage:
  migod [OPTIONS]

The daemon takes all of its configuration from the environment and TOML files
(the MIGO_* variables and the configuration directory); there are no
configuration flags to pass here.

Options:
  -h, --help       Print this help text and exit
  -V, --version    Print version information and exit
";

/// Parses the arguments *after* the program name into a [`Command`].
///
/// `main` passes `std::env::args().skip(1)`; a test passes whatever slice it wants to prove. The
/// first recognised request wins: `-h`/`--help` yields [`Command::Help`] and `-V`/`--version`
/// yields [`Command::Version`], so `migod --help --version` prints help. Every argument is still
/// examined, and any other one — an unknown flag or a stray positional — is rejected with a
/// [`UsageError`] rather than being silently ignored, even when it trails a recognised flag: a
/// daemon that quietly accepts a misspelled flag is a daemon that quietly ignores what the
/// operator meant. No argument at all is [`Command::Serve`].
///
/// # Errors
///
/// Returns a [`UsageError`] naming the first argument that is not one of the recognised flags.
pub fn parse<I>(args: I) -> Result<Command, UsageError>
where
    I: IntoIterator<Item = String>,
{
    let mut request = None;
    for arg in args {
        let recognised = match arg.as_str() {
            "-h" | "--help" => Command::Help,
            "-V" | "--version" => Command::Version,
            _ => return Err(UsageError { arg }),
        };
        request = request.or(Some(recognised));
    }
    Ok(request.unwrap_or(Command::Serve))
}
