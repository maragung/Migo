//! Ephemeral state for Migo.
//!
//! Two backends behind one set of traits: an in-process one for tests, simulation,
//! and single-node development, and Redis for everything real.
//!
//! Everything here is reconstructible. Presence, typing, session routing and rate
//! limit counters live in this crate; nothing durable does, which is the split ADR-0004
//! settled and brief section 158 states. The practical consequence is a rule callers
//! must honour: **losing the cache degrades a request, it does not fail one.** See
//! [`traits`] for what that means method by method, and `docs/runbooks/redis-loss.md`
//! for what an operator sees when it happens.
//!
//! Domain crates should depend on the narrow traits — [`traits::PresenceCache`],
//! [`traits::TokenBucketCache`], and so on — never on a concrete backend. [`open`] exists
//! for the composition root, which is the one place allowed to know which backend is
//! running.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod key;
pub mod memory;
pub mod model;
pub mod redis;
pub mod traits;

use std::sync::Arc;

use migo_core::config::{CacheBackend, CacheConfig};
use migo_core::Result;

pub use crate::key::CacheKey;
pub use crate::memory::MemoryCache;
pub use crate::model::{
    BucketSpec, BucketState, BucketVerdict, Counted, PresenceEntry, SessionRoute, Ttl,
};
pub use crate::redis::RedisCache;
pub use crate::traits::Cache;

/// A cache, shared by every request handler.
///
/// `Arc<dyn Cache>` for the same reason `migo_store::SharedStore` is a trait object:
/// the backend is chosen once at startup, and a generic parameter would spread that
/// one decision through every handler and every test.
pub type SharedCache = Arc<dyn Cache>;

/// Builds the configured cache.
///
/// Not async, and it does not connect. Redis is contacted on first use, so a node
/// whose cache is still starting can finish starting itself — a server that
/// crash-loops on an unavailable cache has converted a degradable dependency into a
/// hard one. Call [`Cache::health`] afterwards to find out whether Redis is actually
/// there.
pub fn open(config: &CacheConfig) -> Result<SharedCache> {
    match config.backend {
        CacheBackend::Memory => Ok(Arc::new(MemoryCache::new())),
        CacheBackend::Redis => Ok(Arc::new(RedisCache::connect(config)?)),
    }
}
