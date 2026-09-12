//! The deterministic quality model.
//!
//! # What this is, honestly
//!
//! A pure function from observed link numbers to a rung of the degradation
//! ladder. This is not a congestion controller: nothing here measures a
//! packet, estimates a rate, or reacts to the network. The transport that
//! owns the socket observes its link, calls
//! [`Sfu`](crate::Sfu) with the numbers, and this module classifies them.
//! The classification is the deliverable the brief asks for when it pins the
//! *order* of degradation (section 165) — bitrate, then resolution, then
//! frame rate, then video off with audio kept — and demands recovery in
//! steps rather than jumps, because a jump re-enters the congestion it just
//! escaped and oscillation is what a live controller spends its life
//! damping. The honest deterministic model gets the damping by construction:
//! down is immediate, up is one rung per interval, and neither direction can
//! skip a rung without saying so through the ladder itself.
//!
//! # The score
//!
//! One number, summed from the six inputs the brief names: loss weighted
//! hardest (it is the one input that means packets are gone), RTT above a
//! baseline, jitter, the deficit between what a leg is sent and what it can
//! carry, and dropped frames. The weights are visible arithmetic, not
//! learned anything, and they live behind [`AdaptiveThresholds`] so an
//! operator retunes them per node like every other limit here.

use migo_core::Timestamp;
use migo_protocol::BandwidthMode;

use crate::frame::Layer;
use crate::model::{
    AdaptiveThresholds, LinkStats, QualityStep, SfuConfig, LOW_DATA_FRAME_STRIDE,
    LOW_DATA_MAX_LAYER,
};

/// Sums one link report into a single congestion score.
///
/// Saturating at every step: a report full of impossible numbers produces the
/// worst score, not a wrapped one that reads as a good network.
#[must_use]
pub fn congestion_score(stats: &LinkStats, thresholds: &AdaptiveThresholds) -> u32 {
    let mut score = stats.packet_loss_pct.saturating_mul(4);
    score = score.saturating_add(stats.rtt_ms.saturating_sub(thresholds.rtt_baseline_ms) / 10);
    score = score.saturating_add(stats.jitter_ms / 5);
    let deficit_kbps = stats.sent_kbps.saturating_sub(stats.available_kbps);
    score = score.saturating_add(deficit_kbps / 100);
    score = score.saturating_add(stats.dropped_frame_pct / 2);
    score
}

/// The rung a link report says the subscription belongs on.
///
/// The ladder's order and the thresholds' order are the same order, so a
/// worse score can only name a lower rung: the classification cannot skip
/// bitrate and jump to resolution, because the rungs it would skip are the
/// ones with lower thresholds.
#[must_use]
pub fn target_step(stats: &LinkStats, thresholds: &AdaptiveThresholds) -> QualityStep {
    let score = congestion_score(stats, thresholds);
    if score >= thresholds.video_off_at {
        QualityStep::VideoOff
    } else if score >= thresholds.frame_rate_at {
        QualityStep::FrameRateLowered
    } else if score >= thresholds.resolution_at {
        QualityStep::ResolutionLowered
    } else if score >= thresholds.bitrate_at {
        QualityStep::BitrateCapped
    } else {
        QualityStep::Full
    }
}

/// Moves a subscription from its current rung toward the target.
///
/// Down is immediate: a saturated link helps nobody, and the ladder's order
/// is the order of what is given up, so landing directly on a low rung has
/// passed through the same sacrifices in the same sequence. Up is one rung
/// at a time, and only after [`SfuConfig::ramp_interval_ms`] has elapsed
/// since the last move — never a jump, because a jump is the oscillation the
/// brief forbids. A target above the current rung that arrives too soon
/// changes nothing.
#[must_use]
pub fn advance(
    current: QualityStep,
    target: QualityStep,
    changed_at: Timestamp,
    now: Timestamp,
    ramp_interval_ms: i64,
) -> QualityStep {
    if target <= current {
        return target;
    }
    let interval = ramp_interval_ms.max(1) as u64;
    if now.saturating_since(changed_at) >= interval {
        QualityStep::at(current.index() + 1)
    } else {
        current
    }
}

/// What the forwarder does to one subscriber's copy of a stream, after the
/// subscriber's request, the adaptive rung, and the subscriber's bandwidth
/// mode have each had their say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForwardShape {
    /// The video layer forwarded, or `None` when no video is.
    pub video: Option<Layer>,
    /// How many frames are dropped: one in every `frame_stride` is
    /// forwarded, by sequence.
    pub frame_stride: u32,
    /// The bitrate the publisher is asked to hold, as a percentage of what
    /// it sends, or `None` when uncapped.
    pub bitrate_cap_pct: Option<u32>,
    /// The keyframe cadence the publisher is asked to keep, milliseconds.
    pub keyframe_interval_ms: i64,
}

/// Resolves one subscriber's forwarding shape.
///
/// Three ceilings, one minimum each. The requested layer is what the
/// subscriber asked for; the adaptive rung is what their link can carry; the
/// bandwidth mode is what their whole device has asked the network to spare
/// them (section 75 and section 165). LowData caps the layer below HD and
/// halves the frame rate for everyone in the mode; UltraLowData does the
/// same and additionally treats *any* degradation as "the network cannot
/// carry video" — video goes off and audio keeps running, which is the
/// mode's whole promise.
///
/// Audio never passes through here: an audio stream is forwarded to every
/// subscriber of it, full stop. The ladder gives up video on its worst rung
/// precisely so that it never has to give up audio.
#[must_use]
pub fn shape(
    requested: Layer,
    quality: QualityStep,
    mode: BandwidthMode,
    config: &SfuConfig,
) -> ForwardShape {
    let mut out = ForwardShape {
        video: quality.layer().map(|layer| layer.min(requested)),
        frame_stride: quality.frame_stride(),
        bitrate_cap_pct: quality.bitrate_cap_pct(),
        keyframe_interval_ms: config.keyframe_interval_ms,
    };
    if matches!(mode, BandwidthMode::LowData | BandwidthMode::UltraLowData) {
        out.video = out.video.map(|layer| layer.min(LOW_DATA_MAX_LAYER));
        out.frame_stride = out.frame_stride.max(LOW_DATA_FRAME_STRIDE);
        out.keyframe_interval_ms = config.low_data_keyframe_interval_ms;
        if mode == BandwidthMode::UltraLowData && quality != QualityStep::Full {
            // The mode's rule (section 165): when the network is not
            // adequate, video is dropped and audio continues. Any rung below
            // the top is "not adequate" here — UltraLowData does not sit
            // through a degraded picture, it stops sending one.
            out.video = None;
        }
    }
    out
}
