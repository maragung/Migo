//! Configuration, identities, and the answers the media plane gives.
//!
//! # Product limits are configuration, not protocol constants
//!
//! Section 165 pins the defaults — thirty-two audio participants, eight
//! active video streams — and says outright that they are product limits an
//! operator retunes per node as SFU capacity allows. They therefore arrive in
//! [`SfuConfig`] and are validated at construction, the same startup-time
//! refusal the rate limiter applies to a bucket that could never serve its
//! operation: a node configured for more video than it has seats for should
//! refuse to start rather than discover the arithmetic at two in the morning.

use migo_core::{Id, Timestamp};
use migo_protocol::fault;

use crate::frame::Layer;

/// The product default for how many participants one call holds (section 165).
///
/// Thirty-two seats, each present as audio; over the limit a join is answered
/// `QUOTA_EXCEEDED`. A limit, not a promise: an operator with a bigger node
/// raises it in [`SfuConfig`], because the number is a capacity statement
/// about hardware and not a fact about the protocol.
pub const MAX_AUDIO_PARTICIPANTS: usize = 32;

/// The product default for how many video streams may be active at once
/// (section 165).
///
/// Eight. Everyone else in a full call is present as audio with their video
/// paused, which is a different thing from being refused: the ninth publish
/// is answered `QUOTA_EXCEEDED` while the participant keeps their seat and
/// their audio, and a slot freed by an unpublish is theirs to take.
pub const MAX_ACTIVE_VIDEO_STREAMS: usize = 8;

/// The product default for how many streams one participant may subscribe to.
pub const MAX_SUBSCRIPTIONS_PER_PARTICIPANT: usize = 16;

/// How long the subscription-churn window runs, milliseconds.
pub const SUBSCRIBE_WINDOW_MS: i64 = 10_000;

/// How many subscription requests one participant may make per window before
/// the answer is `RATE_LIMITED`.
pub const SUBSCRIBE_WINDOW_MAX: u32 = 20;

/// How long a recovering subscription must wait before it may climb one rung,
/// milliseconds (section 165: quality rises in steps, never in a jump,
/// because a jump re-enters the congestion it just escaped and oscillates).
pub const RAMP_INTERVAL_MS: i64 = 3_000;

/// The keyframe cadence a publisher is asked to keep, milliseconds.
///
/// An advisory, and deliberately one: a frame's being a keyframe is a fact
/// about its plaintext, and the only thing this crate knows about a frame's
/// plaintext is that it must not know. The number travels to the client,
/// which is the party that can act on it.
pub const KEYFRAME_INTERVAL_MS: i64 = 2_000;

/// The cadence asked for under LowData (section 165: keyframe frequency is
/// reduced, so the ask is longer, not shorter).
pub const LOW_DATA_KEYFRAME_INTERVAL_MS: i64 = 8_000;

/// The highest layer forwarded under LowData: HD off (section 165).
pub const LOW_DATA_MAX_LAYER: Layer = Layer::Medium;

/// The finest frame rate forwarded under LowData: every second frame is
/// dropped, by sequence parity, which needs no clock and no plaintext.
pub const LOW_DATA_FRAME_STRIDE: u32 = 2;

/// The bitrate the publisher is asked to hold once the first rung of the
/// ladder is reached, as a percentage of what it sends.
pub const BITRATE_CAP_PCT: u32 = 60;

/// The tighter percentage asked for once frame rate is being given up too.
pub const LOW_BITRATE_CAP_PCT: u32 = 40;

/// One seated participant: an account on a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Member {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the media arrives on. A seat belongs to a device the
    /// same way the signalling roster's does: a second device of the same
    /// account takes the seat rather than sitting beside it, or one person
    /// would hold two seats and receive every frame twice.
    pub device_id: Id,
}

/// What a stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// Voice. Never on the degradation ladder, never capped, never dropped
    /// for bandwidth — the one stream every rung of the ladder keeps.
    Audio,
    /// A simulcast video stream, selected per subscriber by layer.
    Video,
}

/// A publisher's declaration of one stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRequest {
    /// Publisher-minted stream id, and the publish's idempotency key: the
    /// same id with the same definition is the same stream, and the same id
    /// with a different definition is refused rather than redefined.
    pub stream_id: Id,
    /// Audio or video.
    pub kind: StreamKind,
    /// The simulcast layers the publisher encodes. Video offers one to three,
    /// unique; audio offers none, because voice is not simulcast and an
    /// audio stream carries the single implicit [`Layer::Low`].
    pub layers: Vec<Layer>,
}

/// Where a subscriber's video stands on the degradation ladder.
///
/// The rungs are the brief's pinned order (section 165): when the network
/// worsens, bitrate goes first, then resolution, then frame rate, and only
/// then video — audio is never on this ladder because audio is never given
/// up; that is the "Very poor" rung's whole meaning. The derived order is
/// the ladder order, so comparing two steps says which is further down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityStep {
    /// The requested layer, every frame, at the publisher's full bitrate.
    Full,
    /// The same layer and stride, with a reduced-bitrate request to the
    /// publisher. The SFU cannot re-encode a sealed frame, so this rung is
    /// an advisory the client honours — the first thing given up is the one
    /// the forwarder cannot take by itself.
    BitrateCapped,
    /// A lower simulcast layer: resolution, and with it the bitrate a bigger
    /// picture costs.
    ResolutionLowered,
    /// The lowest layer, with every second frame dropped by sequence: frame
    /// rate is the third thing to go.
    FrameRateLowered,
    /// Video off. Audio keeps flowing; this is where a bad call is still a
    /// call.
    VideoOff,
}

impl QualityStep {
    /// The ladder, top rung first.
    pub const LADDER: [Self; 5] = [
        Self::Full,
        Self::BitrateCapped,
        Self::ResolutionLowered,
        Self::FrameRateLowered,
        Self::VideoOff,
    ];

    /// The video layer this rung forwards, if it forwards video.
    #[must_use]
    pub const fn layer(self) -> Option<Layer> {
        match self {
            Self::Full | Self::BitrateCapped => Some(Layer::High),
            Self::ResolutionLowered => Some(Layer::Medium),
            Self::FrameRateLowered => Some(Layer::Low),
            Self::VideoOff => None,
        }
    }

    /// How many frames are skipped between forwarded ones: with a stride of
    /// two, every second frame by sequence is dropped. Frame rate is the
    /// third sacrifice, and dropping by sequence parity is how a forwarder
    /// that cannot see plaintext still lowers a frame rate.
    #[must_use]
    pub const fn frame_stride(self) -> u32 {
        match self {
            Self::FrameRateLowered => 2,
            Self::Full | Self::BitrateCapped | Self::ResolutionLowered | Self::VideoOff => 1,
        }
    }

    /// The bitrate the publisher is asked to stay under, as a percentage of
    /// what it is sending. `None` when uncapped — or when there is no video
    /// left to cap.
    #[must_use]
    pub const fn bitrate_cap_pct(self) -> Option<u32> {
        match self {
            Self::Full | Self::VideoOff => None,
            Self::BitrateCapped | Self::ResolutionLowered => Some(BITRATE_CAP_PCT),
            Self::FrameRateLowered => Some(LOW_BITRATE_CAP_PCT),
        }
    }

    /// Position on the ladder, [`QualityStep::Full`] at zero. Crate-private:
    /// the number is the ladder-walk's arithmetic, not a fact about quality a
    /// caller should reason with.
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// The rung at a position, clamped to the ladder's ends. Crate-private
    /// for the same reason [`QualityStep::index`](Self::index) is: only the
    /// one-rung-at-a-time walk in [`crate::adaptive`] may name a position.
    pub(crate) const fn at(index: usize) -> Self {
        if index >= Self::LADDER.len() {
            Self::VideoOff
        } else {
            Self::LADDER[index]
        }
    }
}

/// The numbers a subscriber's transport observes about its own downlink, the
/// ones section 165 names: packet loss, RTT, jitter, available bandwidth,
/// bitrate sent, dropped frames.
///
/// Reported by the client, believed only as far as the ladder: a lying report
/// can beg for quality it cannot carry, which wastes the liar's own
/// bandwidth and nobody else's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Loss on the reporting leg, whole percents.
    pub packet_loss_pct: u32,
    /// Round-trip time, milliseconds.
    pub rtt_ms: u32,
    /// Jitter, milliseconds.
    pub jitter_ms: u32,
    /// Bandwidth the leg can still carry, kilobits per second.
    pub available_kbps: u32,
    /// What the leg is currently being sent, kilobits per second.
    pub sent_kbps: u32,
    /// Frames the receiver dropped, whole percents.
    pub dropped_frame_pct: u32,
}

/// Where a link's congestion score crosses each rung of the ladder.
///
/// Defaults are tuning, not truth, and they are configuration for the same
/// reason the seat counts are: what a node's network considers "poor" is a
/// fact about the node, not about the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdaptiveThresholds {
    /// RTT below which latency contributes nothing to the score.
    pub rtt_baseline_ms: u32,
    /// The score at which bitrate is capped.
    pub bitrate_at: u32,
    /// The score at which resolution drops.
    pub resolution_at: u32,
    /// The score at which frame rate drops.
    pub frame_rate_at: u32,
    /// The score at which video turns off and only audio remains.
    pub video_off_at: u32,
}

impl Default for AdaptiveThresholds {
    fn default() -> Self {
        Self {
            rtt_baseline_ms: 120,
            bitrate_at: 12,
            resolution_at: 25,
            frame_rate_at: 40,
            video_off_at: 60,
        }
    }
}

impl AdaptiveThresholds {
    /// Refuses a ladder that cannot be walked in order.
    ///
    /// The thresholds must rise strictly, because the ladder is walked by
    /// comparing one score against five of them: a threshold out of order
    /// would make a worse network appear to deserve a better rung, and that
    /// is not a tuning mistake the forwarding path could notice.
    pub fn validate(&self) -> migo_core::Result<()> {
        if self.rtt_baseline_ms == 0 {
            return Err(fault::validation(
                "rtt_baseline_ms",
                "the baseline must be at least one millisecond",
            ));
        }
        if self.bitrate_at == 0
            || self.resolution_at <= self.bitrate_at
            || self.frame_rate_at <= self.resolution_at
            || self.video_off_at <= self.frame_rate_at
        {
            return Err(fault::validation(
                "adaptive",
                "thresholds must rise strictly: bitrate, resolution, frame rate, video off",
            ));
        }
        Ok(())
    }
}

/// Everything this media plane refuses to hard-code.
#[derive(Clone, Debug, PartialEq)]
pub struct SfuConfig {
    /// How many participants a call holds. Default
    /// [`MAX_AUDIO_PARTICIPANTS`].
    pub max_audio_participants: usize,
    /// How many video streams may be active at once. Default
    /// [`MAX_ACTIVE_VIDEO_STREAMS`].
    pub max_active_video_streams: usize,
    /// How many streams one participant may subscribe to. Default
    /// [`MAX_SUBSCRIPTIONS_PER_PARTICIPANT`].
    pub max_subscriptions_per_participant: usize,
    /// The subscription-churn window, milliseconds. Default
    /// [`SUBSCRIBE_WINDOW_MS`].
    pub subscribe_window_ms: i64,
    /// Subscription requests allowed per window. Default
    /// [`SUBSCRIBE_WINDOW_MAX`].
    pub subscribe_window_max: u32,
    /// How long a recovering subscription waits before climbing one rung.
    /// Default [`RAMP_INTERVAL_MS`].
    pub ramp_interval_ms: i64,
    /// The keyframe cadence asked of publishers. Default
    /// [`KEYFRAME_INTERVAL_MS`].
    pub keyframe_interval_ms: i64,
    /// The cadence asked under LowData. Default
    /// [`LOW_DATA_KEYFRAME_INTERVAL_MS`].
    pub low_data_keyframe_interval_ms: i64,
    /// Where the adaptive ladder's rungs sit. Default
    /// [`AdaptiveThresholds::default`].
    pub adaptive: AdaptiveThresholds,
}

impl Default for SfuConfig {
    fn default() -> Self {
        Self {
            max_audio_participants: MAX_AUDIO_PARTICIPANTS,
            max_active_video_streams: MAX_ACTIVE_VIDEO_STREAMS,
            max_subscriptions_per_participant: MAX_SUBSCRIPTIONS_PER_PARTICIPANT,
            subscribe_window_ms: SUBSCRIBE_WINDOW_MS,
            subscribe_window_max: SUBSCRIBE_WINDOW_MAX,
            ramp_interval_ms: RAMP_INTERVAL_MS,
            keyframe_interval_ms: KEYFRAME_INTERVAL_MS,
            low_data_keyframe_interval_ms: LOW_DATA_KEYFRAME_INTERVAL_MS,
            adaptive: AdaptiveThresholds::default(),
        }
    }
}

impl SfuConfig {
    /// Refuses a configuration this node could not honestly serve.
    ///
    /// Run at construction, before the first frame: an impossible limit is a
    /// deployment error, and a deployment error should stop the deploy, not
    /// surface as the call that behaved strangely at capacity.
    pub fn validate(&self) -> migo_core::Result<()> {
        if self.max_audio_participants < 2 {
            return Err(fault::validation(
                "max_audio_participants",
                "a call needs at least two seats",
            ));
        }
        if self.max_active_video_streams == 0
            || self.max_active_video_streams > self.max_audio_participants
        {
            return Err(fault::validation(
                "max_active_video_streams",
                "between one and the number of seats",
            ));
        }
        if self.max_subscriptions_per_participant == 0 {
            return Err(fault::validation(
                "max_subscriptions_per_participant",
                "at least one subscription per participant",
            ));
        }
        if self.subscribe_window_ms <= 0 || self.subscribe_window_max == 0 {
            return Err(fault::validation(
                "subscribe_window",
                "a positive window and a positive allowance",
            ));
        }
        if self.ramp_interval_ms <= 0 {
            return Err(fault::validation(
                "ramp_interval_ms",
                "recovery must wait a positive interval between rungs",
            ));
        }
        if self.keyframe_interval_ms <= 0 || self.low_data_keyframe_interval_ms <= 0 {
            return Err(fault::validation(
                "keyframe_interval_ms",
                "a positive cadence for both modes",
            ));
        }
        self.adaptive.validate()
    }
}

/// What a join came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    /// Seated; the call holds one participant more than before.
    Joined,
    /// The same device joined again: the same seat, answered idempotently,
    /// and nothing about the call changed.
    Rejoined,
}

/// What a leave came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveOutcome {
    /// The seat is gone, and with it every stream it published and every
    /// subscription it held.
    Left,
    /// There was no seat to leave. Success all the same — a leave is a
    /// request that the caller be absent, and they are.
    Absent,
}

/// What a publish came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The stream is published; a video stream holds one of the call's
    /// active-video slots until it is unpublished or its publisher leaves.
    Published,
    /// The same stream id with the same definition: the first publish
    /// stands, answered idempotently.
    Republished,
}

/// What an unpublish came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnpublishOutcome {
    /// The stream is gone; a video slot was freed.
    Left,
    /// No such stream on this seat. Success: the state asked for already
    /// holds.
    Absent,
}

/// What a subscribe came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscribeOutcome {
    /// Held, at the requested layer.
    Granted,
    /// The same subscription, moved to a new requested layer.
    Relayered,
}

/// What an unsubscribe came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsubscribeOutcome {
    /// Dropped.
    Left,
    /// Never held. Success: the state asked for already holds.
    Absent,
}

/// What the media plane decided after one stats report, for one
/// subscription.
///
/// The fields split honestly into what this plane enforces itself —
/// [`Adaptation::layer`] and [`Adaptation::frame_stride`] are applied by the
/// forwarder, frame by frame — and what it can only ask for:
/// [`Adaptation::bitrate_cap_pct`] and [`Adaptation::keyframe_interval_ms`]
/// are requests to a publisher that holds the only key that could act on
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Adaptation {
    /// The rung the subscription now stands on.
    pub quality: QualityStep,
    /// The video layer actually forwarded, after the subscriber's request,
    /// the adaptive rung, and the subscriber's bandwidth mode each took
    /// their share. `None` when no video is forwarded.
    pub layer: Option<Layer>,
    /// How many frames are dropped: one frame in every `frame_stride` is
    /// forwarded.
    pub frame_stride: u32,
    /// The bitrate the publisher is asked to hold, as a percentage of what
    /// it sends, or `None` when uncapped.
    pub bitrate_cap_pct: Option<u32>,
    /// The keyframe cadence the publisher is asked to keep, milliseconds.
    pub keyframe_interval_ms: i64,
    /// Whether this report moved the subscription to a different rung.
    pub changed: bool,
}

/// A timestamp, named: every mutating call carries the caller's `now`, and
/// nothing in this crate reads a clock of its own.
///
/// The media plane is deterministic by construction — a test drives time by
/// handing the plane the same `Timestamp` it will later assert against, and
/// there is no sleep anywhere to race.
pub type Now = Timestamp;
