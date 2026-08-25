//! The single funnel every error leaves through.
//!
//! A handler or an extractor produces a [`migo_core::Error`]; this wraps it as [`ApiError`] and
//! turns it into an HTTP response exactly one way. Two brief rules are enforced here and nowhere
//! else, so they cannot be got wrong per-handler:
//!
//! - The HTTP status is derived from the error code by the generated table (brief section 118),
//!   not chosen by hand at the call site. A code's status is a property of the code.
//! - Only the error's public face crosses the wire (section 161). [`migo_core::Error`] keeps an
//!   internal message for the server's own logs and a separate public message for the client;
//!   this serialises the public one, which is empty for a server-fault error — a client learns
//!   *that* something failed, never the internals of *why*.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use migo_core::Error;
use migo_protocol::fault;

/// A [`migo_core::Error`] on its way to becoming an HTTP response.
///
/// Handlers return `Result<T, ApiError>` and extractors use it as their rejection, so every
/// failure path — a bad token, a validation failure, a refused rate-limit charge, a storage
/// outage — lands in the same [`IntoResponse`] and comes out shaped identically.
pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

/// The JSON envelope: `{ "error": { … } }`, one object so a client can branch on the presence of
/// the `error` key alone.
#[derive(Serialize)]
struct Envelope<'a> {
    error: Body<'a>,
}

/// The error body a client receives. The `code` is the stable machine identifier, the `symbol`
/// its name for a human reading logs, and the `message` the public sentence — never the internal
/// one.
#[derive(Serialize)]
struct Body<'a> {
    code: u32,
    symbol: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u32>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let error = self.0;
        let status = StatusCode::from_u16(fault::http_status(error.code()))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_after = error.retry_after();
        let body = Envelope {
            error: Body {
                code: error.code(),
                symbol: error.symbol(),
                message: error.public_message(),
                retry_after_ms: retry_after,
            },
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(millis) = retry_after {
            let seconds = (millis / 1000).max(1);
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}
