//! Who is online.
//!
//! Presence is the cheapest thing in a chat product to get wrong and the most
//! expensive thing to get wrong at scale: it changes constantly, it interests
//! everybody who knows the user, and it is the one signal people read as a
//! statement about a person rather than about a socket. This crate gives four
//! guarantees.
//!
//! **A device is online for as long as it was told to prove it.** Entries live
//! [`MISSED_HEARTBEATS`] heartbeats, and the heartbeat is the one the gateway
//! advertised to *that* session — so a client on a metered connection, which was
//! told to heartbeat slowly, is not punished for obeying.
//!
//! **Invisible means the server does not say.** Brief section 14 puts the
//! enforcement on the server. Invisible is projected to Offline before any frame
//! exists, so there is no code path that could leak it and no client cooperation
//! required.
//!
//! **A person has one state, not one per device.** The account's visible state is
//! the strongest thing any of its live devices claims, and a deliberate Busy is
//! never overridden by another device's automatic Online.
//!
//! **Nothing is sent when nothing changed.** Brief section 156. A heartbeat from a
//! steady device, a re-declaration of the state already held, and the disconnect of
//! one device among several all produce no frame at all.
//!
//! # Delivery is not here
//!
//! Every mutating operation returns an [`Option<Fanout>`](Fanout): a description of
//! one change and whose topic it belongs to. The gateway encodes it once and sends
//! it to every subscriber. See the [`fanout`] module for why a domain crate that
//! delivered its own frames would be a second gateway with a cache in the middle.
//!
//! # Example
//!
//! ```no_run
//! # use migo_core::metrics::Registry;
//! # use migo_presence::PresenceConfig;
//! # fn wiring(
//! #     store: migo_store::SharedStore,
//! #     cache: migo_cache::SharedCache,
//! #     limiter: migo_ratelimit::SharedRateLimiter,
//! #     registry: &Registry,
//! #     gateway: &migo_core::config::GatewayConfig,
//! # ) {
//! // The heartbeat comes from the gateway that will advertise it, so the lifetime
//! // of a presence entry and the interval a client was told to use cannot drift.
//! let config = PresenceConfig::from_gateway(gateway);
//! let presence = migo_presence::open(store, cache, limiter, registry, config);
//! # let _ = presence;
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod fanout;
mod metrics;
pub mod model;
pub mod service;
pub mod state;
pub mod traits;

pub use crate::fanout::Fanout;
pub use crate::model::{
    cadence_for, Cadence, Caller, Detail, PresenceConfig, PresenceScope, MAX_HEARTBEAT_MS,
    MAX_LAST_SEEN_LOOKUPS, MAX_SNAPSHOT_SUBJECTS, MIN_HEARTBEAT_MS, MISSED_HEARTBEATS,
};
pub use crate::service::{open, Presences, SharedPresence};
pub use crate::state::{declared_state, visible_state};
pub use crate::traits::Presence;
