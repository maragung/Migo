//! Logging and tracing setup.
//!
//! Two output shapes, chosen by configuration: a human format for developers and
//! newline-delimited JSON for production, where a log line is a record consumed
//! by a machine rather than prose read by a person.
//!
//! The rule that matters is not the format but the content. Structured fields
//! only — `tracing::info!(session_id = %id, "resumed")`, never
//! `format!("session {id} resumed")` — because the former is queryable and the
//! latter is a haystack. And nothing that would be a privacy incident if it
//! leaked: no message bodies, no tokens, no keys, no full phone numbers. See
//! `docs/03-security-threat-model.md`.

use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// How log records should be rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Compact, coloured, aimed at a terminal.
    #[default]
    Pretty,
    /// One JSON object per line, aimed at a log pipeline.
    Json,
}

/// Installs the global subscriber.
///
/// `directives` follows `RUST_LOG` syntax and is overridden by the `RUST_LOG`
/// environment variable when set, so an operator can raise the log level on a
/// running deployment without editing configuration.
///
/// Returns an error if a subscriber is already installed; callers in tests
/// should ignore it rather than panic, since only the first test to run wins.
pub fn init(directives: &str, format: LogFormat) -> Result<(), String> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(directives))
        .map_err(|error| format!("invalid log filter {directives:?}: {error}"))?;

    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_target(true),
            )
            .try_init()
            .map_err(|error| error.to_string()),
        LogFormat::Pretty => registry
            .with(fmt::layer().with_target(true).with_ansi(supports_colour()))
            .try_init()
            .map_err(|error| error.to_string()),
    }
}

/// Best-effort colour detection: honour `NO_COLOR`, otherwise assume a terminal.
fn supports_colour() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_malformed_filter() {
        // Guard the error path; the success path installs a process-global and
        // therefore cannot be asserted twice in one test binary.
        let result = init("this is not=a=filter", LogFormat::Pretty);
        assert!(result.is_err());
    }

    #[test]
    fn log_format_round_trips_through_serde() {
        let json = serde_json::to_string(&LogFormat::Json).expect("serializes");
        assert_eq!(json, "\"json\"");
        let parsed: LogFormat = serde_json::from_str("\"pretty\"").expect("deserializes");
        assert_eq!(parsed, LogFormat::Pretty);
    }
}
