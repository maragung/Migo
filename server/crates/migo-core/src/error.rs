//! The error type every layer returns.
//!
//! One shape, three audiences, and keeping them apart is the whole point:
//!
//! * **The client** gets a numeric code, a stable symbol, and only the detail
//!   we explicitly marked safe to disclose. A validation error may name the
//!   offending field; a storage error may not name the table.
//! * **The log** gets the internal message and the source chain.
//! * **The metric** gets the code, which is a bounded label — unlike a message
//!   string, which is not (`docs/09-observability-ops.md`).
//!
//! Defaulting to non-disclosure is deliberate. Every "helpful" error message
//! that leaked a query, a path, or an internal hostname started life as a
//! convenience.

use std::fmt;

/// Coarse class of an error, mirroring the protocol's error classes.
///
/// The class decides what a client should *do*; the code decides what to *say*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The peer broke the wire contract. Not retryable; the session closes.
    Protocol,
    /// Credentials are missing, expired, or revoked. Re-authenticate.
    Auth,
    /// Authenticated, but not allowed. Surface the denial; do not retry.
    Permission,
    /// The request is malformed at the semantic level. A client bug.
    Validation,
    /// Quota exceeded. Retry after the advertised delay.
    RateLimit,
    /// The request conflicts with current state. Reconcile, then retry.
    State,
    /// We failed. Retry with backoff.
    Server,
    /// A remote server failed. Degrade and retry.
    Federation,
}

impl ErrorKind {
    /// Whether a well-behaved client should retry the same request.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::RateLimit | ErrorKind::Server | ErrorKind::Federation
        )
    }

    /// Whether the error is our fault. Drives the `migo_errors_total` split
    /// between "we broke" and "they asked for something impossible", which is
    /// the difference between paging someone and not.
    #[must_use]
    pub fn is_our_fault(self) -> bool {
        matches!(self, ErrorKind::Server | ErrorKind::Federation)
    }

    /// Stable lowercase name, used as a metric label and log field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Protocol => "protocol",
            ErrorKind::Auth => "auth",
            ErrorKind::Permission => "permission",
            ErrorKind::Validation => "validation",
            ErrorKind::RateLimit => "rate_limit",
            ErrorKind::State => "state",
            ErrorKind::Server => "server",
            ErrorKind::Federation => "federation",
        }
    }
}

/// A failure, carrying everything the three audiences need and nothing more.
pub struct Error {
    kind: ErrorKind,
    code: u32,
    symbol: &'static str,
    /// Written to logs. Never sent to a client.
    internal: String,
    /// Explicitly cleared for disclosure by the code that produced the error.
    public_detail: Option<String>,
    retry_after_ms: Option<u32>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Error {
    /// Builds an error. `code` and `symbol` come from the generated protocol
    /// tables so that the wire value and the log symbol can never drift.
    #[must_use]
    pub fn new(
        kind: ErrorKind,
        code: u32,
        symbol: &'static str,
        internal: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            code,
            symbol,
            internal: internal.into(),
            public_detail: None,
            retry_after_ms: None,
            source: None,
        }
    }

    /// Attaches detail that is safe to send to the peer.
    #[must_use]
    pub fn public(mut self, detail: impl Into<String>) -> Self {
        self.public_detail = Some(detail.into());
        self
    }

    /// Attaches a retry delay. Only meaningful for retryable kinds.
    #[must_use]
    pub fn retry_after_ms(mut self, millis: u32) -> Self {
        self.retry_after_ms = Some(millis);
        self
    }

    /// Attaches the underlying cause for the log's benefit.
    #[must_use]
    pub fn caused_by(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The error class.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The numeric wire code.
    #[must_use]
    pub fn code(&self) -> u32 {
        self.code
    }

    /// The stable symbol, e.g. `RATE_LIMITED`.
    #[must_use]
    pub fn symbol(&self) -> &'static str {
        self.symbol
    }

    /// The advertised retry delay, if any.
    #[must_use]
    pub fn retry_after(&self) -> Option<u32> {
        self.retry_after_ms
    }

    /// The message for logs. Contains internals; never send this to a peer.
    #[must_use]
    pub fn internal_message(&self) -> &str {
        &self.internal
    }

    /// The message for the peer: only what was explicitly disclosed.
    #[must_use]
    pub fn public_message(&self) -> &str {
        self.public_detail.as_deref().unwrap_or("")
    }

    /// True when a client should retry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}/{}]", self.internal, self.symbol, self.code)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("Error");
        s.field("kind", &self.kind)
            .field("code", &self.code)
            .field("symbol", &self.symbol)
            .field("internal", &self.internal);
        if let Some(detail) = &self.public_detail {
            s.field("public_detail", detail);
        }
        if let Some(retry) = &self.retry_after_ms {
            s.field("retry_after_ms", retry);
        }
        if let Some(source) = &self.source {
            s.field("source", &format_args!("{source}"));
        }
        s.finish()
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|boxed| &**boxed as &(dyn std::error::Error + 'static))
    }
}

/// Shorthand for the crate-wide result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_detail_is_not_disclosed_by_default() {
        let err = Error::new(
            ErrorKind::Server,
            1600,
            "INTERNAL",
            "insert into messages failed: connection refused at 10.0.0.7:5432",
        );
        assert_eq!(err.public_message(), "");
        assert!(err.internal_message().contains("10.0.0.7"));
    }

    #[test]
    fn disclosure_is_explicit() {
        let err = Error::new(
            ErrorKind::Validation,
            1301,
            "FIELD_TOO_LONG",
            "body 90000 > 65536",
        )
        .public("field 'body' exceeds the maximum length");
        assert_eq!(
            err.public_message(),
            "field 'body' exceeds the maximum length"
        );
    }

    #[test]
    fn retryability_follows_the_class() {
        assert!(ErrorKind::RateLimit.is_retryable());
        assert!(ErrorKind::Server.is_retryable());
        assert!(!ErrorKind::Permission.is_retryable());
        assert!(!ErrorKind::Protocol.is_retryable());
    }

    #[test]
    fn blame_split_drives_alerting() {
        assert!(ErrorKind::Server.is_our_fault());
        assert!(!ErrorKind::Validation.is_our_fault());
    }

    #[test]
    fn source_chain_is_preserved() {
        let io = std::io::Error::new(std::io::ErrorKind::TimedOut, "pool checkout timed out");
        let err =
            Error::new(ErrorKind::Server, 1602, "STORE_UNAVAILABLE", "store failed").caused_by(io);
        assert!(std::error::Error::source(&err).is_some());
    }
}
