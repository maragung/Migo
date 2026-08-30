//! 1:1 call signalling: the ring lifecycle and the sealed relay.
//!
//! # What this crate is
//!
//! The server's entire role in a Migo call is *state and routing*: it holds
//! which call is ringing, for whom, until when; it moves sealed SDP and ICE
//! between the two devices; and it tells each party what the other did. The
//! media itself is device-to-device, end-to-end encrypted with keys the
//! server never holds — so the one thing this crate must never become is
//! clever about payload. [`Callkeeper::relay_sdp`] and
//! [`Callkeeper::relay_ice`] read three ids off each frame, check them
//! against the call row, and pass the bytes through unopened.
//!
//! ```ignore
//! let calls = migo_calls::open(store, limiter, gate, &registry, CallsConfig::default());
//! let (outcome, event) = calls.invite(&caller, invite).await?;
//! // The dispatcher replies with the outcome and publishes the event to the
//! // callee's user topic; the service never sends a frame itself.
//! ```
//!
//! # The lifecycle
//!
//! `Ringing` (the invite, with a deadline) → `Connecting` (the callee's
//! answer) → `Connected` (the first sealed answer relayed) → `Ended` (with a
//! reason). There is no path back: a reconnect is a client-side media
//! matter, and a re-ring is a new call id. Every path that leaves a live
//! state writes an end — the decline, the cancel, the end, the answer that
//! arrived one millisecond late, and the sweep that retires whatever the
//! deadlines killed — so the store cannot accumulate a ring nobody can stop.
//!
//! # What this crate is not
//!
//! *Not an SFU.* Group calls are a separate deployment; the opcodes for them
//! are answered `FEATURE_DISABLED` upstream and nothing here knows they
//! exist.
//!
//! *Not a TURN service.* Credentials come from operator configuration;
//! [`Callkeeper::turn_servers`] returns what was configured and nothing
//! more.
//!
//! *Not the notifier.* Whether a ringing call should wake an offline device
//! is `migo-notify`'s decision, made from the event the dispatcher
//! publishes — not from in here, where no connection context exists.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod metrics;
pub mod model;
pub mod service;
pub mod store;
pub mod traits;

pub use model::{
    Call, CallIceWire, CallInviteWire, CallSdpWire, CallState, Caller, CallsConfig, EndReason,
    InviteOutcome, TurnServerWire, MAX_SEALED_LEN, MEDIA_AUDIO, MEDIA_VIDEO, RING_TTL_MS,
};
pub use service::{open, Calls};
pub use store::{CallStore, MemoryCallStore, SharedCallStore};
pub use traits::{CallGate, Callkeeper, OpenGate, SharedCallGate, SharedCallkeeper};
