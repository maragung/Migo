//! Durable storage for Migo.
//!
//! Two backends behind one set of traits: an in-memory one for tests, simulation,
//! and local development, and PostgreSQL for everything real.
//!
//! Domain crates should depend on the narrow traits in [`traits`] — `AccountStore`,
//! `MessagingStore`, and so on — never on a concrete backend. [`open`] exists for
//! the composition root, which is the one place that is allowed to know which
//! backend is running.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub(crate) mod entity;
pub mod memory;
mod migration;
pub mod model;
pub mod postgres;
pub mod traits;

use std::sync::Arc;

use migo_core::config::{StoreBackend, StoreConfig};
use migo_core::{Result, Secret};
use migo_protocol::fault;

pub use crate::memory::MemoryStore;
pub use crate::postgres::PostgresStore;
pub use crate::traits::{Store, MAX_PAGE};

/// A store, shared by every request handler.
///
/// `Arc<dyn Store>` rather than a generic parameter: the backend is chosen once at
/// startup, and threading a `S: Store` through every handler, service, and test
/// would spread that one decision across the whole codebase for no benefit.
pub type SharedStore = Arc<dyn Store>;

/// Builds the configured store.
///
/// Async, and it still does not connect. The pool is lazy on purpose, so `migod` can
/// finish starting while the database is still coming up instead of crash-looping
/// against it; call [`Store::migrate`] and [`Store::health`] afterwards to find out
/// whether the database is actually there. What the `async` buys is the URL being
/// parsed and rejected here, at startup, rather than on the first query — SeaORM's
/// constructor is async even when it is told not to connect, and an operator wants a
/// typo in a connection string reported before the process claims to be up.
pub async fn open(config: &StoreConfig) -> Result<SharedStore> {
    match config.backend {
        StoreBackend::Memory => Ok(Arc::new(MemoryStore::new())),
        StoreBackend::Postgres => {
            if config.url.as_ref().is_none_or(Secret::is_empty) {
                // `Config::validate` already rejects this at startup; the check is
                // repeated because failing here beats the alternatives. Falling back
                // to the memory backend would run the whole server on non-durable
                // storage, and unwrapping the missing URL would panic a worker thread.
                return Err(fault::validation(
                    "store.url",
                    "is required for the postgres backend",
                ));
            }
            Ok(Arc::new(PostgresStore::connect(config).await?))
        }
    }
}
