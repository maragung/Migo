//! Building a [`migo_core::Error`] from a protocol error code.
//!
//! Every layer above this one raises failures by code: `fault::not_found("room")`
//! rather than `Error::new(ErrorKind::State, 1500, "NOT_FOUND", ...)`. The point
//! is that the code, the symbol, the behavioural class, and the HTTP status are
//! all derived from the generated tables in one place. Hand-assembling them at
//! each call site is how a code ends up logged as one symbol and sent as another,
//! and that divergence is invisible until someone is debugging at 3 a.m.
//!
//! # On what reaches the client
//!
//! [`migo_core::Error`] discloses nothing by default: the internal message goes
//! to logs, and only [`migo_core::Error::public`] detail is sent to the peer. The
//! helpers here follow the same rule — the `what` argument is an internal note,
//! not a disclosure. Where a message *is* safe and useful to a client (a field
//! name in a validation error, say), the call site adds it explicitly with
//! `.public(...)`, and that explicitness is the feature.

use migo_core::{Error, ErrorKind};

use crate::generated::{codes, error_symbol, ErrorClass};

/// Behavioural class of a code, as [`ErrorKind`].
///
/// An unknown code maps to [`ErrorKind::Server`]: a code this build does not
/// recognise means our own table is out of date, which is our fault, not the
/// caller's.
#[must_use]
pub fn kind_of(code: u32) -> ErrorKind {
    match ErrorClass::of(code) {
        ErrorClass::Protocol => ErrorKind::Protocol,
        ErrorClass::Auth => ErrorKind::Auth,
        ErrorClass::Permission => ErrorKind::Permission,
        ErrorClass::Validation => ErrorKind::Validation,
        ErrorClass::RateLimit => ErrorKind::RateLimit,
        ErrorClass::State => ErrorKind::State,
        ErrorClass::Federation => ErrorKind::Federation,
        ErrorClass::Server | ErrorClass::Unknown => ErrorKind::Server,
    }
}

/// Builds an error from a code and an internal message.
///
/// The symbol comes from the generated table. A code missing from that table
/// still produces a usable error rather than a panic — a broken error path must
/// not be the thing that takes the process down.
#[must_use]
pub fn error(code: u32, internal: impl Into<String>) -> Error {
    let symbol = error_symbol(code).unwrap_or("UNKNOWN_ERROR");
    Error::new(kind_of(code), code, symbol, internal)
}

/// The HTTP status a REST handler should return for a code.
#[must_use]
pub fn http_status(code: u32) -> u16 {
    crate::generated::error_http_status(code)
}

/// Something was asked for that does not exist, or that the caller may not know
/// exists.
///
/// The second half matters: a private room the caller cannot see returns this,
/// not `PERMISSION_DENIED`, because "you may not view this room" confirms the
/// room exists. Enumeration attacks are built out of exactly that difference.
#[must_use]
pub fn not_found(what: &str) -> Error {
    error(codes::NOT_FOUND, format!("{what} not found"))
}

/// The caller is authenticated but not permitted.
#[must_use]
pub fn permission_denied(what: &str) -> Error {
    error(
        codes::PERMISSION_DENIED,
        format!("permission denied: {what}"),
    )
}

/// The caller is not authenticated at all.
#[must_use]
pub fn unauthenticated(why: &str) -> Error {
    error(codes::UNAUTHENTICATED, format!("unauthenticated: {why}"))
}

/// Credentials were supplied and were wrong.
///
/// Deliberately the same error for "no such user" and "wrong password". Any
/// difference between the two is a free account-existence oracle, and rate
/// limiting does not close it — an attacker only needs one request per guess.
#[must_use]
pub fn invalid_credentials() -> Error {
    error(codes::INVALID_CREDENTIALS, "invalid credentials")
        .public("Username or password is incorrect")
}

/// The request is malformed at the semantic level. `field` is disclosed, because
/// a client cannot fix a validation error it cannot locate.
#[must_use]
pub fn validation(field: &str, why: &str) -> Error {
    error(
        codes::VALIDATION_FAILED,
        format!("validation failed on {field}: {why}"),
    )
    .public(format!("{field}: {why}"))
}

/// A required field was absent.
#[must_use]
pub fn field_required(field: &str) -> Error {
    error(codes::FIELD_REQUIRED, format!("missing field {field}")).public(field.to_string())
}

/// A field exceeded its documented limit.
#[must_use]
pub fn field_too_long(field: &str, limit: usize) -> Error {
    error(
        codes::FIELD_TOO_LONG,
        format!("field {field} exceeds {limit} bytes"),
    )
    .public(format!("{field} exceeds {limit} bytes"))
}

/// The request conflicts with existing state.
#[must_use]
pub fn conflict(what: &str) -> Error {
    error(codes::CONFLICT, format!("conflict: {what}"))
}

/// The thing being created already exists.
#[must_use]
pub fn already_exists(what: &str) -> Error {
    error(codes::ALREADY_EXISTS, format!("{what} already exists"))
}

/// The caller cannot afford it.
///
/// `what` names the currency for the log. The shortfall is deliberately not in the
/// public detail: a client that wants to show "you need 40 more coins" already knows
/// the price and can fetch the balance, and a server that volunteered the number in
/// an error would be answering `BALANCE_FETCH` from an endpoint that was never
/// authorised to.
#[must_use]
pub fn insufficient_balance(what: &str) -> Error {
    error(
        codes::INSUFFICIENT_BALANCE,
        format!("insufficient {what} balance"),
    )
}

/// Quota exceeded, with the delay a well-behaved client should wait.
#[must_use]
pub fn rate_limited(retry_after_ms: u32) -> Error {
    error(codes::RATE_LIMITED, "rate limited")
        .retry_after_ms(retry_after_ms)
        .public(format!(
            "Too many requests. Retry in {} s",
            retry_after_ms.div_ceil(1000)
        ))
}

/// We failed. The message is for the log; the client learns only that it can
/// retry.
#[must_use]
pub fn internal(what: impl Into<String>) -> Error {
    error(codes::INTERNAL_ERROR, what)
}

/// Durable storage is unreachable or refused the operation.
#[must_use]
pub fn storage(what: impl Into<String>) -> Error {
    error(codes::STORAGE_UNAVAILABLE, what)
}

/// The cache is unreachable. Callers should degrade rather than fail where the
/// cached value is reconstructible, which by [ADR-0004] it always is.
///
/// [ADR-0004]: https://github.com/migo/migo/blob/main/docs/adr/0004-postgres-redis-s3.md
#[must_use]
pub fn cache(what: impl Into<String>) -> Error {
    error(codes::CACHE_UNAVAILABLE, what)
}

/// The process is shutting down and declined new work.
#[must_use]
pub fn shutting_down() -> Error {
    error(codes::SHUTTING_DOWN, "shutting down").retry_after_ms(1_000)
}

/// A feature is disabled by its kill switch.
#[must_use]
pub fn feature_disabled(feature: &str) -> Error {
    error(
        codes::FEATURE_DISABLED,
        format!("feature disabled: {feature}"),
    )
}

/// The peer sent something the wire contract does not allow.
#[must_use]
pub fn malformed_frame(why: impl Into<String>) -> Error {
    error(codes::MALFORMED_FRAME, why)
}

/// A decodable frame arrived in a session state that does not accept it.
#[must_use]
pub fn unexpected_opcode(opcode: u32, state: &str) -> Error {
    error(
        codes::UNEXPECTED_OPCODE,
        format!("opcode {opcode} is not accepted while {state}"),
    )
}

/// Translates a wire-level codec failure into a protocol error.
///
/// The `WireError` text describes a shape violation and never quotes payload
/// bytes, so it is safe in a log. It is still not disclosed to the peer: a
/// client that cannot encode a frame correctly does not get a decoder oracle to
/// debug against.
#[must_use]
pub fn from_wire(source: migo_wire::WireError) -> Error {
    let code = match &source {
        migo_wire::WireError::FrameTooLarge { .. } => codes::FRAME_TOO_LARGE,
        migo_wire::WireError::UnsupportedVersion { .. } => codes::PROTOCOL_VERSION_UNSUPPORTED,
        migo_wire::WireError::ReservedFlags { .. } => codes::UNSUPPORTED_FLAG,
        _ => codes::DECODE_FAILED,
    };
    error(code, format!("wire decode failed: {source}")).caused_by(source)
}

// --- federation (section 169) ---------------------------------------------

/// A peer node could not be reached to deliver a federated event.
///
/// Retryable, and it names no address in anything a client sees: which node is
/// unreachable is an operational fact for the log, not something a caller — or a
/// peer watching how the mesh answers — gets to read off an error.
#[must_use]
pub fn peer_unreachable(what: impl Into<String>) -> Error {
    error(codes::PEER_UNREACHABLE, what)
}

/// A region's mesh links are degraded and the request cannot be served there now.
///
/// Retryable: the caller should fall back to another region or wait, not treat the
/// work as failed.
#[must_use]
pub fn region_degraded(region: &str) -> Error {
    error(codes::REGION_DEGRADED, format!("region degraded: {region}"))
}

/// A room is readable but cannot accept writes right now, because the partition
/// that owns it is unreachable from here.
///
/// A partition must not fork a room's history into two divergent tails, so the
/// side that does not own the room refuses writes until the link heals (section
/// 169). Reads still succeed from the local replica.
#[must_use]
pub fn room_read_only_partition() -> Error {
    error(
        codes::ROOM_READ_ONLY_PARTITION,
        "room is read-only during a mesh partition",
    )
}

/// A mesh handshake failed. Deliberately one error for every reason it can fail.
///
/// An unknown node, a bad signature, a skewed or stale timestamp, an unsupported
/// protocol version, a replayed nonce — every one of them is this, with the same
/// code, the same symbol, and no public detail at all. Section 169 requires the
/// handshake to fail closed and the link to be dropped; section 48's same-error
/// rule requires that a peer probing the mesh cannot tell *why* it was turned
/// away, because the gap between "I do not know you" and "your signature was
/// wrong" is an oracle worth closing. The `why` is for our log and nothing else.
#[must_use]
pub fn mesh_auth_failed(why: impl Into<String>) -> Error {
    error(codes::MESH_AUTH_FAILED, why)
}

/// A routing decision was made against an epoch older than the current one.
///
/// The caller should refetch the routing table and retry. A `409` rather than a
/// `503`: the request was well-formed against a view of the mesh that has since
/// moved on, which is a conflict to reconcile, not a fault of this node.
#[must_use]
pub fn routing_epoch_stale(what: impl Into<String>) -> Error {
    error(codes::ROUTING_EPOCH_STALE, what)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_class_maps_to_a_kind() {
        // Each range's low bound, so a new class added to the generator without a
        // mapping here fails rather than silently becoming `Server`.
        assert_eq!(kind_of(codes::MALFORMED_FRAME), ErrorKind::Protocol);
        assert_eq!(kind_of(codes::UNAUTHENTICATED), ErrorKind::Auth);
        assert_eq!(kind_of(codes::PERMISSION_DENIED), ErrorKind::Permission);
        assert_eq!(kind_of(codes::VALIDATION_FAILED), ErrorKind::Validation);
        assert_eq!(kind_of(codes::RATE_LIMITED), ErrorKind::RateLimit);
        assert_eq!(kind_of(codes::NOT_FOUND), ErrorKind::State);
        assert_eq!(kind_of(codes::INTERNAL_ERROR), ErrorKind::Server);
        assert_eq!(kind_of(codes::PEER_UNREACHABLE), ErrorKind::Federation);
    }

    #[test]
    fn an_unknown_code_is_our_fault_not_theirs() {
        let err = error(999_999, "from the future");
        assert_eq!(err.kind(), ErrorKind::Server);
        assert_eq!(err.symbol(), "UNKNOWN_ERROR");
        assert!(err.is_retryable());
    }

    #[test]
    fn every_generated_code_has_a_symbol() {
        for code in codes::ALL {
            assert!(error_symbol(*code).is_some(), "code {code} has no symbol");
            assert_ne!(error(*code, "x").symbol(), "UNKNOWN_ERROR");
        }
    }

    #[test]
    fn the_symbol_always_matches_the_code() {
        let err = not_found("room");
        assert_eq!(err.code(), codes::NOT_FOUND);
        assert_eq!(err.symbol(), "NOT_FOUND");
    }

    #[test]
    fn internal_detail_is_not_disclosed() {
        let err = storage("connection to 10.0.0.7:5432 refused");
        assert!(err.internal_message().contains("10.0.0.7"));
        assert_eq!(
            err.public_message(),
            "",
            "an address must never reach a client"
        );
    }

    #[test]
    fn wrong_password_and_no_such_user_are_indistinguishable() {
        // Same code, same symbol, same public text. The oracle stays closed.
        let a = invalid_credentials();
        let b = invalid_credentials();
        assert_eq!(a.code(), b.code());
        assert_eq!(a.public_message(), b.public_message());
        assert!(!a.public_message().to_lowercase().contains("user not found"));
    }

    #[test]
    fn a_validation_error_names_the_field() {
        let err = validation("username", "must be 3 to 20 characters");
        assert!(err.public_message().starts_with("username:"));
        assert_eq!(err.kind(), ErrorKind::Validation);
        assert!(!err.is_retryable());
    }

    #[test]
    fn rate_limiting_advertises_a_delay_and_rounds_it_up() {
        let err = rate_limited(1_500);
        assert_eq!(err.retry_after(), Some(1_500));
        assert!(
            err.public_message().contains("2 s"),
            "{}",
            err.public_message()
        );
        assert!(err.is_retryable());
    }

    #[test]
    fn wire_errors_keep_their_specific_codes() {
        assert_eq!(
            from_wire(migo_wire::WireError::FrameTooLarge { len: 2, max: 1 }).code(),
            codes::FRAME_TOO_LARGE
        );
        assert_eq!(
            from_wire(migo_wire::WireError::UnsupportedVersion {
                found: 9,
                supported: 1
            })
            .code(),
            codes::PROTOCOL_VERSION_UNSUPPORTED
        );
        assert_eq!(
            from_wire(migo_wire::WireError::TrailingBytes { count: 3 }).code(),
            codes::DECODE_FAILED
        );
    }

    #[test]
    fn http_status_comes_from_the_generated_table() {
        assert_eq!(http_status(codes::NOT_FOUND), 404);
        assert_eq!(http_status(codes::RATE_LIMITED), 429);
        assert_eq!(http_status(codes::INTERNAL_ERROR), 500);
        assert_eq!(http_status(codes::PERMISSION_DENIED), 403);
    }

    #[test]
    fn a_mesh_handshake_failure_is_opaque_to_the_peer() {
        // Every reason it can fail must be indistinguishable from the outside: same
        // code, same symbol, and nothing in the public detail (section 48, 169).
        let unknown = mesh_auth_failed("no such node in the allow-list");
        let bad_sig = mesh_auth_failed("signature did not verify");
        assert_eq!(unknown.code(), codes::MESH_AUTH_FAILED);
        assert_eq!(unknown.code(), bad_sig.code());
        assert_eq!(unknown.symbol(), bad_sig.symbol());
        assert_eq!(unknown.public_message(), bad_sig.public_message());
        assert_eq!(
            unknown.public_message(),
            "",
            "no reason may leak to the peer"
        );
        // The distinguishing detail is kept, but only for our own log.
        assert!(unknown.internal_message().contains("allow-list"));
        // A mesh handshake fault is a federation-class code, not an end-user
        // credential failure: MESH_AUTH_FAILED is 1703, and 1700..=1799 is the
        // federation range, so it groups with the other mesh faults for alerting.
        // The "auth" in the name is about authenticating a peer node, not the caller.
        assert_eq!(unknown.kind(), ErrorKind::Federation);
    }

    #[test]
    fn federation_codes_carry_their_class_and_retryability() {
        // 1700-series is the federation class; the reachability faults are retryable
        // so a client shows "degraded" and tries again, while a stale epoch is a
        // conflict to reconcile rather than a transient failure.
        assert_eq!(kind_of(codes::PEER_UNREACHABLE), ErrorKind::Federation);
        assert_eq!(kind_of(codes::REGION_DEGRADED), ErrorKind::Federation);
        assert_eq!(
            kind_of(codes::ROOM_READ_ONLY_PARTITION),
            ErrorKind::Federation
        );
        assert!(peer_unreachable("node 7").is_retryable());
        assert!(region_degraded("eu-west").is_retryable());
        assert!(room_read_only_partition().is_retryable());
        assert_eq!(http_status(codes::ROUTING_EPOCH_STALE), 409);
        assert_eq!(
            routing_epoch_stale("epoch 4 < 5").code(),
            codes::ROUTING_EPOCH_STALE
        );
    }
}
