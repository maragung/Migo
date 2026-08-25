//! Conversations, messages, receipts, sync, and typing.
//!
//! # What this crate is responsible for
//!
//! Ordering and durability, and nothing about content. A message arrives as a
//! sealed envelope, is given the next sequence in its conversation, is stored, and
//! is described to whoever needs to deliver it. Every rule in here is a rule about
//! *when* and *whether*, never about *what*: the server has no key, so it cannot
//! filter, index, translate, or moderate a payload, and no future feature request
//! can change that without changing the product.
//!
//! Four guarantees are what the rest of the system builds on:
//!
//! * **A sequence is assigned once, per conversation, by the store.** Gapless,
//!   monotonic, never reused, and kept by a tombstone — so a client can detect
//!   missing history by arithmetic rather than by asking (brief section 67).
//! * **A retry is a success.** The same `message_id` twice returns the original
//!   with `duplicate: true`, because the client that retried never saw the first
//!   answer and an error would make it report a failure for a delivered message
//!   (section 68).
//! * **Nothing is sent when nothing changed.** A receipt for an already-known
//!   sequence, a delete of an existing tombstone, and a create that found an
//!   existing conversation all produce no broadcast (section 156).
//! * **Cursors, not offsets.** History and the conversation list both page from a
//!   position, so a conversation that receives a message mid-page cannot make a
//!   row appear twice or vanish.
//!
//! # Delivery is somebody else's job
//!
//! Every method that changes something a member can see returns an optional
//! [`Fanout`] — a description of who should hear about it — instead of sending
//! anything. The gateway takes that plan, encodes the event once, and hands the
//! same refcounted buffer to every socket. Delivering from here would mean
//! encoding per recipient, and in a group of two hundred that is two hundred
//! serialisations of one message.
//!
//! It also keeps the boundary honest in the other direction: this crate holds no
//! sockets, so it can be tested with a store and a clock, and every ordering rule
//! it enforces is checked without a network.
//!
//! # Layering
//!
//! Layer 3, so it depends on the kernel and the platform and on no other domain
//! crate. That is why [`Caller`] exists rather than an import of `migo-auth`'s
//! request context: the gateway authenticates, then translates, and the two crates
//! stay independently testable and independently deployable.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use migo_core::metrics::Registry;
//! # async fn wiring(
//! #     store: migo_store::SharedStore,
//! #     cache: migo_cache::SharedCache,
//! #     limiter: migo_ratelimit::SharedRateLimiter,
//! #     registry: &Registry,
//! # ) -> migo_core::Result<()> {
//! let messaging = migo_messaging::open(store, cache, limiter, registry);
//! # let _ = messaging;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod cursor;
pub mod fanout;
mod metrics;
pub mod model;
pub mod service;
pub mod traits;

pub use crate::fanout::{Broadcast, Fanout};
pub use crate::model::{
    Caller, DEFAULT_CONVERSATION_PAGE, MAX_EXPIRY_MS, MAX_GROUP_MEMBERS, MEMBER_PREVIEW,
    TYPING_TTL_MS,
};
pub use crate::service::{open, Messages, SharedMessaging};
pub use crate::traits::Messaging;
