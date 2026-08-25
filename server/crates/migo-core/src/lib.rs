//! Shared primitives for every Migo crate.
//!
//! This is layer 0 of the crate graph: it depends on no other Migo crate, and
//! every other crate may depend on it. Anything that lands here must be useful
//! to at least two other crates and must not encode domain policy.
//!
//! See `docs/01-architecture.md` for the layering rules.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod clock;
pub mod config;
pub mod error;
pub mod id;
pub mod metrics;
pub mod random;
pub mod secret;
pub mod shutdown;
pub mod telemetry;
pub mod time;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::Config;
pub use error::{Error, ErrorKind, Result};
pub use id::{Id, IdParseError, PublicId};
pub use random::{OsRandom, Random, SeededRandom};
pub use secret::Secret;
pub use shutdown::Shutdown;
pub use time::{Timestamp, MIGO_EPOCH_MS};
