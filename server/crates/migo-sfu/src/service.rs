//! The forwarding SFU itself.
//!
//! # The four invariants this file exists to hold
//!
//! **No plaintext, structurally.** The payload a publisher hands
//! [`Sfu::forward`] is a [`SealedFrame`](crate::frame::SealedFrame) — opaque
//! by type, with no way out except into a delivery — and this crate links no
//! cryptography, so there is no code here that could open a frame even by
//! accident. The routing metadata beside the bytes (call, publisher, stream,
//! layer, sequence) is everything this plane ever reads, and it is everything
//! a forwarder needs.
//!
//! **Fan-out never loops back.** A frame is offered to every subscriber of
//! its stream except the seat that published it. The publisher's own other
//! subscriptions are untouched; what is refused is the echo.
//!
//! **The limits are the product's, and enforced here.** Thirty-two seats and
//! eight active video streams are defaults an operator retunes per node, and
//! a call over either is answered with the quota error — the ninth video
//! publish refuses the *stream*, not the participant, who keeps their seat
//! and their audio until a slot frees.
//!
//! **Down fast, up slow.** A subscriber's link report can drop their video
//! to any rung of the ladder immediately, but raising it climbs one rung
//! per interval, because a jump up is a jump back into the congestion just
//! escaped (section 165).
//!
//! # What is deliberately not here
//!
//! *No socket.* This crate is the media plane's decision core — registry,
//! selection, limits, shaping — and the transport that carries frames to it
//! and away from it is a deployment's own choice. Nothing here reads a
//! clock, sleeps, or races; every mutating call carries the caller's `now`.
//!
//! *No key, and no opinion about keys.* Rotation is triggered by
//! participants and distributed by signalling; this plane forwards whatever
//! bytes it is given and could not verify a rotation if it wanted to.

use std::collections::HashMap;

use migo_core::metrics::Registry;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::{codes, fault, BandwidthMode};
use parking_lot::Mutex;

use crate::adaptive;
use crate::frame::{Delivery, InboundFrame, Layer};
use crate::limit::WindowCounter;
use crate::metrics::{AdaptKind, DropReason, JoinKind, Meters, PublishKind, SubscribeKind};
use crate::model::{
    Adaptation, JoinOutcome, LeaveOutcome, LinkStats, Member, PublishOutcome, PublishRequest,
    QualityStep, SfuConfig, StreamKind, SubscribeOutcome, UnpublishOutcome, UnsubscribeOutcome,
};

/// One subscription held by one seat.
struct Subscription {
    publisher: Member,
    stream_id: Id,
    requested: Layer,
    quality: QualityStep,
    changed_at: Timestamp,
}

/// One stream published by one seat.
struct PublishedStream {
    stream_id: Id,
    kind: StreamKind,
    layers: Vec<Layer>,
}

/// One seat in one call: a device, its published streams, and its
/// subscriptions.
struct Seat {
    member: Member,
    mode: BandwidthMode,
    streams: Vec<PublishedStream>,
    subs: Vec<Subscription>,
    window: WindowCounter,
}

/// One call's media plane.
#[derive(Default)]
struct CallPlane {
    seats: Vec<Seat>,
}

impl CallPlane {
    /// The seat a device holds, by index.
    fn seat_index_of_device(&self, device_id: Id) -> Option<usize> {
        self.seats
            .iter()
            .position(|s| s.member.device_id == device_id)
    }

    /// The seat an account holds, by index. One seat per account: a second
    /// device of the same account replaces the seat rather than joining
    /// beside it.
    fn seat_index_of_account(&self, account_id: Id) -> Option<usize> {
        self.seats
            .iter()
            .position(|s| s.member.account_id == account_id)
    }

    /// How many video streams are active across the call's seats.
    fn active_video(&self) -> usize {
        self.seats
            .iter()
            .flat_map(|seat| seat.streams.iter())
            .filter(|stream| stream.kind == StreamKind::Video)
            .count()
    }
}

/// The forwarding media plane for sealed group-call frames.
///
/// One instance serves every call on the node. Calls appear when their first
/// participant joins and disappear when their last leaves, so an abandoned
/// call holds no memory and a fresh join cannot collide with state nobody
/// can reach.
///
/// ```ignore
/// let sfu = migo_sfu::Sfu::new(migo_sfu::SfuConfig::default(), &registry)?;
/// sfu.join(call, member, BandwidthMode::Normal, now)?;
/// let deliveries = sfu.forward(call, member, frame)?;
/// // The transport writes each delivery's frame to its `to`; the plane
/// // never touches a socket.
/// ```
pub struct Sfu {
    config: SfuConfig,
    planes: Mutex<HashMap<Id, CallPlane>>,
    meters: Meters,
}

impl Sfu {
    /// Builds the plane, refusing a configuration the node could not serve.
    ///
    /// Validation happens here rather than at first use because an
    /// impossible limit is a deployment error, and the deploy is the moment
    /// to fail (the same reasoning as the rate limiter's startup checks).
    pub fn new(config: SfuConfig, registry: &Registry) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            planes: Mutex::new(HashMap::new()),
            meters: Meters::new(registry),
        })
    }

    /// The configuration this plane enforces.
    #[must_use]
    pub fn config(&self) -> &SfuConfig {
        &self.config
    }

    /// Seats a participant in a call, creating the call if it is the first
    /// seat.
    ///
    /// Idempotent per device: the same device joining again is answered
    /// [`JoinOutcome::Rejoined`] and changes nothing. A *different* device
    /// of the same account takes the seat, dropping the streams and
    /// subscriptions the old device held — a person is one participant, not
    /// one per screen. A call at its seat ceiling is answered
    /// `QUOTA_EXCEEDED`, and a seat-taking join never counts the seat it is
    /// replacing.
    pub fn join(
        &self,
        call_id: Id,
        member: Member,
        mode: BandwidthMode,
        now: Timestamp,
    ) -> Result<JoinOutcome> {
        let mut planes = self.planes.lock();
        let plane = planes.entry(call_id).or_default();
        if let Some(index) = plane.seat_index_of_account(member.account_id) {
            if plane.seats[index].member == member {
                plane.seats[index].mode = mode;
                self.meters.join(JoinKind::Duplicate);
                return Ok(JoinOutcome::Rejoined);
            }
            // A different device of the same account takes the seat, and the
            // streams and subscriptions the old device held go with it.
            plane.seats.remove(index);
        } else if plane.seats.len() >= self.config.max_audio_participants {
            self.meters.join(JoinKind::Quota);
            return Err(quota("the call holds its full complement of participants"));
        }
        plane.seats.push(Seat {
            member,
            mode,
            streams: Vec::new(),
            subs: Vec::new(),
            window: WindowCounter::new(
                self.config.subscribe_window_ms,
                self.config.subscribe_window_max,
                now,
            ),
        });
        self.meters.join(JoinKind::Joined);
        self.refresh_load(&planes);
        Ok(JoinOutcome::Joined)
    }

    /// Vacates a seat, with every stream it published, every subscription it
    /// held, and every subscription any other seat held to its streams.
    ///
    /// A leave from a device that holds no seat is
    /// [`LeaveOutcome::Absent`] and still a success: a leave is a request
    /// that the caller be absent, and they are. The last leave retires the
    /// call's plane entirely.
    pub fn leave(&self, call_id: Id, member: Member) -> Result<LeaveOutcome> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Ok(LeaveOutcome::Absent);
        };
        let Some(index) = plane.seat_index_of_device(member.device_id) else {
            return Ok(LeaveOutcome::Absent);
        };
        plane.seats.remove(index);
        // Every subscription pointed at the departed seat goes too: a
        // subscription to a stream nobody can publish is unreachable by the
        // forwarder (which re-checks the publisher's seat), but leaving it
        // held would keep it answerable on the stats path and counted
        // against the subscriber's ceiling.
        for other in plane.seats.iter_mut() {
            other.subs.retain(|sub| sub.publisher != member);
        }
        if plane.seats.is_empty() {
            planes.remove(&call_id);
        }
        self.meters.leave();
        self.refresh_load(&planes);
        Ok(LeaveOutcome::Left)
    }

    /// Declares one of the seat's streams.
    ///
    /// The stream id is the publish's idempotency key: the same id with the
    /// same definition is the same stream, answered
    /// [`PublishOutcome::Republished`]; the same id with a different
    /// definition is `IDEMPOTENCY_MISMATCH`, because redefining a stream
    /// under a subscriber's feet is what the refusal exists for. A video
    /// publish past the call's active-video ceiling is refused with
    /// `QUOTA_EXCEEDED` *and nothing else happens to the participant*: they
    /// keep their seat, their audio, and their subscriptions, and may take
    /// a slot the moment one frees.
    pub fn publish(
        &self,
        call_id: Id,
        member: Member,
        request: PublishRequest,
    ) -> Result<PublishOutcome> {
        let layers = offered_layers(&request)?;
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Err(fault::not_found("call"));
        };
        let Some(seat_index) = plane.seat_index_of_device(member.device_id) else {
            return Err(fault::not_found("seat"));
        };
        let active_video = plane.active_video();
        let seat = &mut plane.seats[seat_index];
        if let Some(existing) = seat
            .streams
            .iter()
            .find(|s| s.stream_id == request.stream_id)
        {
            if existing.kind == request.kind && existing.layers == layers {
                self.meters.publish(PublishKind::Republished);
                return Ok(PublishOutcome::Republished);
            }
            return Err(fault::error(
                codes::IDEMPOTENCY_MISMATCH,
                "stream id reused with a different definition",
            ));
        }
        if request.kind == StreamKind::Video && active_video >= self.config.max_active_video_streams
        {
            self.meters.publish(PublishKind::Quota);
            return Err(quota(
                "the call holds its full complement of active video streams",
            ));
        }
        seat.streams.push(PublishedStream {
            stream_id: request.stream_id,
            kind: request.kind,
            layers,
        });
        self.meters.publish(PublishKind::Published);
        self.refresh_load(&planes);
        Ok(PublishOutcome::Published)
    }

    /// Retires one of the seat's streams, freeing a video slot if it was
    /// video, and dropping every other seat's subscription to it so nothing
    /// dangles.
    pub fn unpublish(
        &self,
        call_id: Id,
        member: Member,
        stream_id: Id,
    ) -> Result<UnpublishOutcome> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Ok(UnpublishOutcome::Absent);
        };
        let Some(seat_index) = plane.seat_index_of_device(member.device_id) else {
            return Ok(UnpublishOutcome::Absent);
        };
        let seat = &mut plane.seats[seat_index];
        let Some(index) = seat.streams.iter().position(|s| s.stream_id == stream_id) else {
            return Ok(UnpublishOutcome::Absent);
        };
        seat.streams.remove(index);
        for other in plane.seats.iter_mut() {
            other
                .subs
                .retain(|sub| !(sub.publisher == member && sub.stream_id == stream_id));
        }
        self.meters.unpublish();
        self.refresh_load(&planes);
        Ok(UnpublishOutcome::Left)
    }

    /// Subscribes a seat to another seat's stream, at a requested layer.
    ///
    /// Two limits guard this path, both from section 165. The churn window
    /// rate-limits how often a participant may ask, answered `RATE_LIMITED`
    /// with the wait — and a refused request still spends the window, so
    /// flooding is never free. The held-subscription ceiling is the quota:
    /// over it, `QUOTA_EXCEEDED`. Re-subscribing to a stream already held
    /// moves its requested layer and does not consume another slot.
    pub fn subscribe(
        &self,
        call_id: Id,
        member: Member,
        publisher: Member,
        stream_id: Id,
        layer: Layer,
        now: Timestamp,
    ) -> Result<SubscribeOutcome> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Err(fault::not_found("call"));
        };
        let Some(seat_index) = plane.seat_index_of_device(member.device_id) else {
            return Err(fault::not_found("seat"));
        };
        if let Some(wait) = plane.seats[seat_index].window.charge(now) {
            self.meters.subscribe(SubscribeKind::RateLimited);
            return Err(fault::rate_limited(wait));
        }
        if publisher.device_id == member.device_id {
            return Err(fault::validation(
                "subscriber",
                "a participant does not subscribe to their own stream",
            ));
        }
        let Some(publisher_index) = plane.seat_index_of_device(publisher.device_id) else {
            return Err(fault::not_found("publisher"));
        };
        let Some(stream) = plane.seats[publisher_index]
            .streams
            .iter()
            .find(|s| s.stream_id == stream_id)
        else {
            return Err(fault::not_found("stream"));
        };
        if !stream.layers.contains(&layer) {
            return Err(fault::validation(
                "layer",
                "the stream does not offer that simulcast layer",
            ));
        }
        let seat = &mut plane.seats[seat_index];
        if let Some(sub) = seat
            .subs
            .iter_mut()
            .find(|s| s.publisher == publisher && s.stream_id == stream_id)
        {
            sub.requested = layer;
            self.meters.subscribe(SubscribeKind::Relayered);
            return Ok(SubscribeOutcome::Relayered);
        }
        if seat.subs.len() >= self.config.max_subscriptions_per_participant {
            self.meters.subscribe(SubscribeKind::Quota);
            return Err(quota(
                "the participant holds their complement of subscriptions",
            ));
        }
        seat.subs.push(Subscription {
            publisher,
            stream_id,
            requested: layer,
            quality: QualityStep::Full,
            changed_at: now,
        });
        self.meters.subscribe(SubscribeKind::Granted);
        Ok(SubscribeOutcome::Granted)
    }

    /// Drops a held subscription. An unsubscribe for a subscription never
    /// held is [`UnsubscribeOutcome::Absent`] and still a success.
    pub fn unsubscribe(
        &self,
        call_id: Id,
        member: Member,
        publisher: Member,
        stream_id: Id,
    ) -> Result<UnsubscribeOutcome> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Ok(UnsubscribeOutcome::Absent);
        };
        let Some(seat_index) = plane.seat_index_of_device(member.device_id) else {
            return Ok(UnsubscribeOutcome::Absent);
        };
        let seat = &mut plane.seats[seat_index];
        let before = seat.subs.len();
        seat.subs
            .retain(|s| !(s.publisher == publisher && s.stream_id == stream_id));
        if seat.subs.len() == before {
            return Ok(UnsubscribeOutcome::Absent);
        }
        self.meters.unsubscribe();
        Ok(UnsubscribeOutcome::Left)
    }

    /// Forwards one sealed frame from a publisher to every other subscriber
    /// of its stream, applying each subscriber's shape.
    ///
    /// The publisher must be a seated device and the stream must be theirs;
    /// anything else is `NOT_FOUND`, the closed enumeration the rest of the
    /// server uses for routing questions. Each subscriber independently
    /// decides whether the frame is theirs: their requested layer, their
    /// adaptive rung, their bandwidth mode, and — for frame rate — the
    /// frame's own sequence number. Audio is forwarded to every subscriber
    /// unconditionally: no rung of the ladder gives up audio, so no branch
    /// of the forwarder may either.
    pub fn forward(&self, call_id: Id, from: Member, frame: InboundFrame) -> Result<Vec<Delivery>> {
        let planes = self.planes.lock();
        let Some(plane) = planes.get(&call_id) else {
            return Err(fault::not_found("call"));
        };
        let Some(seat_index) = plane.seat_index_of_device(from.device_id) else {
            return Err(fault::not_found("seat"));
        };
        let Some(stream) = plane.seats[seat_index]
            .streams
            .iter()
            .find(|s| s.stream_id == frame.stream_id)
        else {
            return Err(fault::not_found("stream"));
        };
        if !stream.layers.contains(&frame.layer) {
            return Err(fault::validation(
                "layer",
                "the stream does not offer that simulcast layer",
            ));
        }
        let mut out = Vec::new();
        for seat in &plane.seats {
            if seat.member == from {
                // Never back to the publisher.
                continue;
            }
            let Some(sub) = seat
                .subs
                .iter()
                .find(|s| s.publisher == from && s.stream_id == frame.stream_id)
            else {
                continue;
            };
            if stream.kind == StreamKind::Audio {
                out.push(delivery(seat.member, from, &frame));
                continue;
            }
            let shape = adaptive::shape(sub.requested, sub.quality, seat.mode, &self.config);
            let Some(layer) = shape.video else {
                self.meters.dropped(DropReason::VideoOff);
                continue;
            };
            if layer != frame.layer {
                self.meters.dropped(DropReason::Layer);
                continue;
            }
            if shape.frame_stride > 1
                && !frame.sequence.is_multiple_of(u64::from(shape.frame_stride))
            {
                self.meters.dropped(DropReason::Stride);
                continue;
            }
            out.push(delivery(seat.member, from, &frame));
        }
        self.meters.forwarded(out.len() as u64);
        Ok(out)
    }

    /// Reports one subscriber's link stats for one subscription and moves
    /// that subscription's rung, down immediately or up one rung per
    /// interval.
    ///
    /// The answer says what the plane will enforce (layer, stride) and what
    /// it asks the publisher to do (bitrate cap, keyframe cadence) — the
    /// split between what a forwarder of sealed frames can take by itself
    /// and what only the keyholder can act on.
    pub fn report_stats(
        &self,
        call_id: Id,
        member: Member,
        publisher: Member,
        stream_id: Id,
        stats: &LinkStats,
        now: Timestamp,
    ) -> Result<Adaptation> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Err(fault::not_found("call"));
        };
        let Some(seat_index) = plane.seat_index_of_device(member.device_id) else {
            return Err(fault::not_found("seat"));
        };
        let mode = plane.seats[seat_index].mode;
        let Some(sub) = plane.seats[seat_index]
            .subs
            .iter_mut()
            .find(|s| s.publisher == publisher && s.stream_id == stream_id)
        else {
            return Err(fault::not_found("subscription"));
        };
        let target = adaptive::target_step(stats, &self.config.adaptive);
        let stepped = adaptive::advance(
            sub.quality,
            target,
            sub.changed_at,
            now,
            self.config.ramp_interval_ms,
        );
        let changed = stepped != sub.quality;
        // The derived order puts the best rung lowest, so a step that
        // compares greater is a step down the ladder.
        self.meters.adapt(if stepped > sub.quality {
            AdaptKind::Lowered
        } else if stepped < sub.quality {
            AdaptKind::Raised
        } else {
            AdaptKind::Held
        });
        if changed {
            sub.quality = stepped;
            sub.changed_at = now;
        }
        let shape = adaptive::shape(sub.requested, sub.quality, mode, &self.config);
        Ok(Adaptation {
            quality: sub.quality,
            layer: shape.video,
            frame_stride: shape.frame_stride,
            bitrate_cap_pct: shape.bitrate_cap_pct,
            keyframe_interval_ms: shape.keyframe_interval_ms,
            changed,
        })
    }

    /// Sets a participant's bandwidth mode, which changes what is forwarded
    /// to them from the next frame on (section 75 and section 165).
    pub fn set_bandwidth_mode(
        &self,
        call_id: Id,
        member: Member,
        mode: BandwidthMode,
    ) -> Result<()> {
        let mut planes = self.planes.lock();
        let Some(plane) = planes.get_mut(&call_id) else {
            return Err(fault::not_found("call"));
        };
        let Some(index) = plane.seat_index_of_device(member.device_id) else {
            return Err(fault::not_found("seat"));
        };
        plane.seats[index].mode = mode;
        Ok(())
    }

    /// Recomputes the load gauges after a mutation that can change them.
    fn refresh_load(&self, planes: &HashMap<Id, CallPlane>) {
        let participants: usize = planes.values().map(|plane| plane.seats.len()).sum();
        let active_video: usize = planes.values().map(|plane| plane.active_video()).sum();
        self.meters
            .set_load(participants as i64, active_video as i64);
    }
}

/// Builds one delivery, cloning the sealed payload for the subscriber it is
/// bound for. The publisher's copy is never handed out.
fn delivery(to: Member, publisher: Member, frame: &InboundFrame) -> Delivery {
    Delivery {
        to,
        publisher,
        stream_id: frame.stream_id,
        sequence: frame.sequence,
        layer: frame.layer,
        frame: frame.payload.clone(),
    }
}

/// The layers a publish offers, normalised.
///
/// Audio carries the single implicit [`Layer::Low`] — voice is not
/// simulcast, and one rule serves every stream. Video offers one to three
/// unique layers, sorted, because a set of simulcast encodings is a set, and
/// the forwarder's layer test is membership.
fn offered_layers(request: &PublishRequest) -> Result<Vec<Layer>> {
    match request.kind {
        StreamKind::Audio => {
            if request.layers.is_empty() || request.layers == [Layer::Low] {
                Ok(vec![Layer::Low])
            } else {
                Err(fault::validation(
                    "layers",
                    "an audio stream has the one implicit layer",
                ))
            }
        }
        StreamKind::Video => {
            if request.layers.is_empty() || request.layers.len() > Layer::VARIANTS {
                return Err(fault::validation(
                    "layers",
                    "a video stream offers between one and three simulcast layers",
                ));
            }
            let mut sorted = request.layers.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() != request.layers.len() {
                return Err(fault::validation(
                    "layers",
                    "simulcast layers must be unique",
                ));
            }
            Ok(sorted)
        }
    }
}

/// The quota error, which names the limit's subject and nothing about any
/// participant.
fn quota(what: &str) -> migo_core::Error {
    fault::error(codes::QUOTA_EXCEEDED, what)
}
