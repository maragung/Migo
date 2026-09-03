//! The social graph: friendships, follows, blocks, favourites, and the privacy gate
//! every other domain asks before it lets one account reach another.
//!
//! # The one method other crates call
//!
//! [`Graph::may_interact`]. Messaging asks it before a direct send, calls ask it before
//! a ring, presence asks it before it discloses a last-seen time. It is the reason this
//! crate exists as a crate rather than as a table three services read: the rule "a
//! block is symmetric, and a pending request is not a friendship" is written once here,
//! and `docs/01-architecture.md` forbids two layer-3 crates from depending on each
//! other, so the composition root wires the answer through instead.
//!
//! ```ignore
//! let social = migo_social::open(store, limiter, &registry, SocialConfig::default());
//! social.may_interact(&caller, recipient, Interaction::Message).await?;
//! ```
//!
//! # What a caller is told when it is refused
//!
//! `PRIVACY_RESTRICTED`, for both "this account blocked you" and "this account's
//! settings exclude you". Brief section 180 requires the two to be indistinguishable,
//! and a system that leaks the difference turns every privacy setting into a way to
//! confirm a suspicion. The caller's *own* block answers `BLOCKED_BY_USER`, which
//! discloses nothing: it tells somebody what they themselves did.
//!
//! # No frames
//!
//! Brief section 145 reserves opcodes 113 to 117 for the social frames and marks the
//! block `STATUS: SPEC`, so none of them is in the generated registry. The mutating
//! methods return a [`Notice`] instead — a `NOTIFICATION_EVENT` (opcode 144) the
//! gateway can already encode — with `title` and `body` left empty so the client writes
//! the sentence in the reader's own language. See [`notice`] for why the server does not
//! write it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod metrics;
pub mod model;
pub mod notice;
pub mod service;
pub mod traits;

pub use model::{
    query_is_usable, strictest, Caller, Edge, Found, FriendOutcome, Interaction, Pending,
    ProfileCard, RespondOutcome, SocialConfig, Standing, Suggestion, DEFAULT_PAGE, MAX_BLOCKS,
    MAX_FAVORITES, MAX_FOLLOWING, MAX_FRIENDS, MAX_MUTES, MAX_MUTUAL_SCAN, MAX_PAGE,
    MAX_PROFILE_BATCH, MAX_QUERY_LEN,
};
pub use notice::Notice;
pub use service::{open, SharedSocial, Social};
pub use traits::Graph;
