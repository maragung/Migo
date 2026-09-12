//! The group-call media plane: a forwarding SFU for sealed frames.
//!
//! # What this crate is
//!
//! Section 166 requires that a call of three or more participants use an
//! SFU, that the SFU only receive and forward encrypted packets, select
//! streams, manage bandwidth, and pass simulcast through, and that it never
//! hold access to plaintext media. Section 92 says the same thing from the
//! deployment side: TURN and SFU do not touch plaintext and therefore do
//! not live inside migod — their load profile is bandwidth, not application
//! logic, and they scale separately. This crate is that media plane: the
//! participant registry, the sealed-frame forwarder, the simulcast
//! selection, the product limits, and the deterministic quality model, as a
//! unit a separate process can own.
//!
//! The MWP opcode table stays closed on purpose (section 146): media frames
//! are not signalling and never ride the MWP wire, so nothing here touches
//! an opcode, a schema table, or the signalling crate. A transport binds
//! this plane to a socket of its own choosing and calls
//! [`Sfu::forward`] with what arrives.
//!
//! # Opacity is a property of the types, not a convention
//!
//! A publisher's payload enters as a [`SealedFrame`]: opaque bytes with no
//! borrow, no slice, and no iterator out — only a consuming hand-off to the
//! transport that will write them. This crate links no cryptography at all,
//! so it holds no call key, could not derive one, and has no code that
//! could open a frame even by accident. The routing metadata beside the
//! bytes is everything the plane reads, and it is everything a forwarder
//! legitimately needs: which call, which publisher, which stream, which
//! simulcast layer, which sequence number.
//!
//! # Determinism
//!
//! Nothing in this crate reads a clock, sleeps, or races. Every mutating
//! call carries the caller's `now`, the adaptive model is a pure function
//! over reported numbers, and frame-rate reduction drops frames by sequence
//! arithmetic rather than by timing. The tests therefore run without a
//! single sleep: they drive time by hand and assert exact behaviour.
//!
//! # What is deliberately not here
//!
//! *No transport.* No socket is opened, framed, or owned; the crate is the
//! decision core a deployment's own listener drives.
//!
//! *No key material.* Rotation is triggered by participants and distributed
//! by the signalling plane; this forwarder could not verify a rotation if
//! it wanted to, which is the point of sealing.
//!
//! *No live congestion control.* The quality model classifies the six
//! numbers section 165 names and walks the ladder it pins; it does not
//! measure anything itself. [`adaptive`] says so in full, and the status
//! lines in the brief say so too.
//!
//! ```ignore
//! let sfu = migo_sfu::Sfu::new(migo_sfu::SfuConfig::default(), &registry)?;
//! sfu.join(call_id, publisher, BandwidthMode::Normal, now)?;
//! sfu.publish(call_id, publisher, video_request)?;
//! sfu.subscribe(call_id, subscriber, publisher, stream_id, Layer::High, now)?;
//! for delivery in sfu.forward(call_id, publisher, frame)? {
//!     // The transport writes delivery.frame to delivery.to.
//! }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod adaptive;
pub mod frame;
mod limit;
mod metrics;
pub mod model;
pub mod service;

pub use adaptive::{congestion_score, shape, target_step, ForwardShape};
pub use frame::{Delivery, InboundFrame, Layer, SealedFrame};
pub use model::{
    Adaptation, AdaptiveThresholds, JoinOutcome, LeaveOutcome, LinkStats, Member, PublishOutcome,
    PublishRequest, QualityStep, SfuConfig, StreamKind, SubscribeOutcome, UnpublishOutcome,
    UnsubscribeOutcome, BITRATE_CAP_PCT, KEYFRAME_INTERVAL_MS, LOW_BITRATE_CAP_PCT,
    LOW_DATA_FRAME_STRIDE, LOW_DATA_KEYFRAME_INTERVAL_MS, LOW_DATA_MAX_LAYER,
    MAX_ACTIVE_VIDEO_STREAMS, MAX_AUDIO_PARTICIPANTS, MAX_SUBSCRIPTIONS_PER_PARTICIPANT,
    RAMP_INTERVAL_MS, SUBSCRIBE_WINDOW_MAX, SUBSCRIBE_WINDOW_MS,
};
pub use service::Sfu;
