//! Public key material: publication and bundle fetch.
//!
//! Layer 3, brief section 163. Two operations, and the reason they are one crate rather
//! than two lines inside the messaging service is that both of them are the *only* points
//! in the system where the server touches key material at all. Keeping them together makes
//! the rule that governs both of them auditable in one place: **no private key, in any
//! parameter, any return field, or any stored row.** There is no type in this crate that
//! could hold one.
//!
//! # What the server is and is not trusted with
//!
//! The server verifies the signed prekey's signature at publication and refuses
//! `INVALID_KEY_MATERIAL` when it does not hold up. That check is not what makes the
//! protocol safe -- the sender verifies the same signature itself, out of the bundle it
//! fetched, and would refuse to send if the server had lied. The server's copy is a
//! data-integrity check: it turns "every message to this device silently fails, forever"
//! into an error at the moment the broken client published, which is the moment somebody
//! can still fix it.
//!
//! Nothing here decides who may talk to whom. A public key is public, and the send itself
//! is gated in `migo_social`; a bundle fetch refused on social grounds would be a second,
//! weaker copy of that gate. What a fetch does cost is a one-time prekey, which is why it
//! is rate limited per account rather than per device.
//!
//! # Layering
//!
//! Domain types in and out -- [`PublishRequest`], [`PublishOutcome`], [`Bundle`],
//! [`Fetched`] -- and layer 4 maps them to `migo_protocol::{KeyPublish, KeyPublishResult,
//! KeyBundleRequest, KeyBundleResponse}`. One field crosses that boundary and vanishes:
//! section 163 lists `signed_prekey_expires_at` on both the publication and the fetched
//! bundle, and the generated wire types have neither, because the IDL and its golden
//! vectors are frozen. So the domain types carry it, the composition root fills it with
//! `now + SIGNED_PREKEY_LIFETIME_MS`, the mapping drops it, and the "refuse an already
//! expired prekey" rule is written against the field -- which means it starts mattering for
//! free the day the IDL catches up.
//!
//! No clock. `Caller::now` is stamped by the caller, at the gateway edge, like every other
//! layer-3 crate in this workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod metrics;
pub mod model;
pub mod service;
pub mod traits;

pub use crate::model::{
    Bundle, Caller, Fetched, KeysConfig, PublishOutcome, PublishRequest, MAX_BUNDLES_PER_FETCH,
    MAX_ONE_TIME_PREKEYS, ONE_TIME_PREKEY_LOW_WATER, SIGNED_PREKEY_LIFETIME_MS,
};
pub use crate::service::{open, Keys, SharedKeyring};
pub use crate::traits::Keyring;
