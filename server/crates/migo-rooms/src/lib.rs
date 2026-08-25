//! Rooms: membership, roles, permissions, and moderation.
//!
//! # What a room is here
//!
//! A room is a conversation with a public identity and a permission table. The
//! conversation part belongs to `migo-messaging` — sequences, history, receipts — and
//! this crate owns everything that decides *who may do what*: who is a member, what
//! role they hold, which bits that role carries, what a moderator took away, and
//! whether a sanction is still in force.
//!
//! The split matters because it is the reason a permission is defined once. Brief
//! section 48 gives nineteen product permissions, and the number of places that could
//! plausibly want to check one — the send path, the call path, the pin button, the
//! bot invocation, the REST moderation endpoints — is large enough that a second
//! implementation would appear within a release. So there is one function,
//! [`permission::resolve`], one order of precedence (role default, plus grant, minus
//! deny), and one entry point for other domains: [`traits::Roomkeeper::authorize`].
//!
//! # How another domain asks
//!
//! `migo-messaging` needs to know whether an account may send into a room, and layer 3
//! crates may not depend on each other (`docs/01-architecture.md`). The way that is
//! resolved is not a shared permission module — it is the composition root asking this
//! crate first:
//!
//! ```text
//! gateway: authorize(caller, room_id, CHAT_SEND) -> Authorized
//! gateway: send(caller, conversation_id, envelope)
//! ```
//!
//! [`model::Authorized`] carries back the conversation id, the room kind, the caller's
//! effective mask, and the slow-mode interval that applies to *them* — so the caller
//! of `authorize` never has to look the room up again, and never has to reimplement
//! the moderator exemption.
//!
//! # What this crate refuses to do
//!
//! **Deliver anything.** Every mutating method returns `Option<`[`Fanout`]`>`: a plan
//! naming the topic, the event, and the one device that already knows. The gateway
//! encodes once and fans out to N sockets. `None` is brief section 156 in the type
//! system — a join into a room you are already in, a settings screen submitted
//! unchanged, and a role set to the one already held all produce no frame.
//!
//! **Count who is online.** `online_count` leaves here as zero, because the number is
//! the size of a subscriber set the gateway holds. See [`view::ONLINE_COUNT_UNSET`].
//!
//! **Enforce slow mode.** The interval is reported; the last-send timestamp lives with
//! the messages.
//!
//! **Cache a membership row.** There is no cache parameter on [`service::open`] at
//! all. A membership row is what decides whether somebody may speak, and a stale copy
//! of one is a member who was banned two minutes ago and is still talking.
//!
//! # Wiring
//!
//! ```no_run
//! # use migo_core::metrics::Registry;
//! # fn wiring(
//! #     store: migo_store::SharedStore,
//! #     limiter: migo_ratelimit::SharedRateLimiter,
//! #     registry: &Registry,
//! #     node: &migo_core::config::NodeConfig,
//! # ) {
//! let config = migo_rooms::RoomsConfig::from_node(node);
//! let rooms = migo_rooms::open(store, limiter, registry, config);
//! # let _ = rooms;
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod fanout;
mod metrics;
pub mod model;
pub mod permission;
pub mod service;
pub mod traits;
pub mod view;

pub use crate::fanout::{Broadcast, Fanout};
pub use crate::model::{
    slug_is_valid, Authorized, Caller, NewRoomRequest, RoomsConfig, Sanction, Settings, TopicChange,
    DEFAULT_LIST_LIMIT, DEFAULT_MAX_MEMBERS, MAX_LIST_LIMIT, MAX_MEMBERS_CEILING, MAX_MUTE_MS,
    MAX_NAME_LEN, MAX_QUERY_LEN, MAX_REASON_LEN, MAX_ROSTER_PAGE, MAX_SLOW_MODE_SECONDS,
    MAX_SLUG_LEN, MAX_TOPIC_LEN, MIN_ROOM_CAPACITY, MIN_SLUG_LEN, PERMANENT_BAN_MS,
};
pub use crate::service::{open, Rooms, SharedRooms};
pub use crate::traits::Roomkeeper;
