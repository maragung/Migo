//! The rate-limit charge applied at the REST edge.
//!
//! The domain authenticator already meters register, sign-in, and refresh per account and per
//! device — it holds the limiter for exactly that. This is the layer in front: a network-scoped
//! charge on the *unauthenticated* bootstrap endpoints, where there is no account yet to meter
//! and the only stable handle on an abuser is the truncated network the request came from
//! (brief sections 120 and 121). It is defence in depth, not the only defence.
//!
//! An anonymous caller is charged at [`TrustTier::Anonymous`] — the tightest budget, which is
//! the right thing to get wrong for a caller who has proven nothing. A request with no known
//! address is not charged: there is no network bucket to charge, and folding every addressless
//! request into one shared bucket would let one such caller rate limit all the others.

use std::net::IpAddr;

use migo_ratelimit::{BucketKey, TrustTier};

use crate::error::ApiError;
use crate::ApiState;

/// Charges one unit against the caller's network bucket, refusing the request if it cannot pay.
///
/// # Errors
///
/// Returns `RATE_LIMITED` (carrying the retry delay) when the network bucket is exhausted, or
/// the limiter's own error if the charge could not be evaluated.
pub(crate) async fn charge_ip(
    state: &ApiState,
    ip: Option<IpAddr>,
    cost: u32,
) -> Result<(), ApiError> {
    let Some(ip) = ip else {
        return Ok(());
    };
    let now = state.now();
    let verdict = state
        .rate_limiter()
        .charge(&[BucketKey::ip(ip)], cost, TrustTier::Anonymous, now)
        .await?;
    verdict.into_result().map_err(ApiError::from)
}
