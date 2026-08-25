//! Abuse control for Migo.
//!
//! One engine, one algorithm, seven surfaces. ADR-0006 settled the shape: a cost-based
//! token bucket, where the price of an operation lives on the opcode in the protocol IDL
//! and the size of a bucket comes from configuration scaled by who is asking and what is
//! being asked of. Brief section 120 lists the surfaces and [`Scope`] has exactly those.
//!
//! ```no_run
//! use std::net::IpAddr;
//! use std::sync::Arc;
//!
//! use migo_core::metrics::Registry;
//! use migo_core::Timestamp;
//! use migo_protocol::Opcode;
//! use migo_ratelimit::{BucketKey, CacheRateLimiter, Policies, RateLimiter, TrustTier};
//!
//! # async fn example(
//! #     cache: Arc<dyn migo_cache::traits::TokenBucketCache>,
//! #     registry: &Registry,
//! #     address: IpAddr,
//! #     account_id: migo_core::Id,
//! #     now: Timestamp,
//! # ) -> migo_core::Result<()> {
//! let limiter = CacheRateLimiter::new(cache, Policies::default(), registry);
//!
//! let keys = [
//!     BucketKey::endpoint_of_account(account_id, Opcode::MessageSend),
//!     BucketKey::account(account_id),
//!     BucketKey::ip(address),
//! ];
//! limiter
//!     .charge_opcode(&keys, Opcode::MessageSend, TrustTier::Established, now)
//!     .await?
//!     .into_result()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Why one engine and not one per feature
//!
//! Because every feature that grows its own limiter grows its own bugs, and they are the
//! same bugs each time: a counter that resets on write instead of on schedule, a
//! `retry_after` that is a guess, a limit that is enforced on one node and not the
//! others, a refusal that costs the attacker nothing. Worse, the limits do not compose —
//! six independent limiters mean an attacker picks whichever is loosest, and nobody can
//! say what the system's actual ceiling is. Here the ceiling is the tightest surface, and
//! it is one line of code that decides it.
//!
//! # Where the state lives
//!
//! In Redis, through [`migo_cache::traits::TokenBucketCache`], which charges a bucket
//! with a Lua script so that a read-modify-write on a hot subject is one atomic round
//! trip rather than a compare-and-set loop that gets less accurate as load rises. When
//! Redis is unreachable the limiter switches to tightened per-node buckets — see
//! [`local`] for what "tightened" buys and what it costs. It never switches off.
//!
//! # What is deliberately not here
//!
//! *No blocklist, no ban list, no captcha.* Those are moderation decisions with an appeal
//! path and an audit trail, and they belong to `migo-moderation`. This crate answers one
//! question — may this operation proceed right now — and forgets the answer immediately.
//!
//! *No per-subject metrics.* An account id as a metric label is an unbounded label, and
//! it is also personal data in a store with no retention policy.
//!
//! *No trust tier computation.* [`TrustTier`] arrives as a parameter. What makes an
//! account trusted is a question about its history, which means database reads, which
//! means the limiter would be doing storage work on the hottest path in the server.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod limiter;
pub mod local;
pub mod metrics;
pub mod policy;
pub mod scope;
pub mod traits;

use std::sync::Arc;

use migo_cache::SharedCache;
use migo_core::config::RateLimitConfig;
use migo_core::metrics::Registry;
use migo_core::Result;

pub use crate::limiter::{CacheRateLimiter, MAX_KEYS_PER_CHARGE};
pub use crate::policy::{max_cost_at, Policies, TrustTier, FALLBACK_DIVISOR};
pub use crate::scope::{network, BucketKey, Scope, TOKEN_FINGERPRINT_BYTES};
pub use crate::traits::{RateLimiter, Verdict};

/// A limiter, shared by every request handler.
pub type SharedRateLimiter = Arc<dyn RateLimiter>;

/// Builds the configured limiter.
///
/// Validates the configuration first, so a budget too small to pay for the operations it
/// governs stops the process at startup instead of returning a permanent `RATE_LIMITED`
/// on one opcode. That is the same posture `migod` takes on a development secret in
/// production: a configuration that cannot work should fail where somebody is watching.
///
/// Takes the whole [`SharedCache`] and narrows it to the one trait this crate touches.
/// The composition root owns the cache; this crate owns nothing but the buckets, and
/// could not read anybody's presence if it wanted to.
pub fn open(
    cache: SharedCache,
    config: &RateLimitConfig,
    registry: &Registry,
) -> Result<SharedRateLimiter> {
    let policies = Policies::from_config(config)?;
    Ok(Arc::new(CacheRateLimiter::new(cache, policies, registry)))
}
