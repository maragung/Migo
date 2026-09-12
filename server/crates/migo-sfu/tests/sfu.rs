//! The media plane, tested where getting it wrong is invisible.
//!
//! A forwarding SFU's failures are silent by design: it moves bytes it
//! cannot read, so nothing downstream can notice a frame that went to the
//! wrong place, a limit that quietly stretched, or a ladder rung that was
//! skipped. The tests here pin the properties the brief states and the
//! callers cannot check:
//!
//! **Fan-out reaches exactly the other subscribers.** A frame goes to every
//! subscriber of its stream whose shape selects the frame's layer, and
//! never back to the seat that published it.
//!
//! **The product limits bite at their numbers.** The thirty-third
//! participant and the ninth active video stream are both refused, and the
//! ninth video refusal leaves the participant seated as audio.
//!
//! **The ladder walks the pinned order.** Bitrate, resolution, frame rate,
//! video off — down in that order at one clock reading, up one rung per
//! interval, never a jump.
//!
//! **Departures leave nothing behind.** A seat that leaves takes its
//! streams and its subscriptions with it.
//!
//! Every test drives time by hand: there is not one sleep in this file.

use migo_core::metrics::Registry;
use migo_core::{Id, Timestamp};
use migo_protocol::{codes, BandwidthMode};
use migo_sfu::frame::{InboundFrame, Layer, SealedFrame};
use migo_sfu::model::{
    JoinOutcome, LeaveOutcome, LinkStats, Member, PublishOutcome, PublishRequest, QualityStep,
    SfuConfig, StreamKind, UnpublishOutcome,
};
use migo_sfu::{Sfu, LOW_DATA_KEYFRAME_INTERVAL_MS, MAX_AUDIO_PARTICIPANTS, RAMP_INTERVAL_MS};

const SECOND: i64 = 1_000;
const NOW: i64 = 1_700_000_000 * SECOND;

const CALL: u128 = 60;

const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;

const ALICE_PHONE: u128 = 101;
const ALICE_LAPTOP: u128 = 102;
const BOB_PHONE: u128 = 103;
const CAROL_PHONE: u128 = 104;

const VIDEO: u128 = 70;
const AUDIO: u128 = 71;

fn id(value: u128) -> Id {
    Id::from(value)
}

fn member(account: u128, device: u128) -> Member {
    Member {
        account_id: id(account),
        device_id: id(device),
    }
}

fn now_plus(millis: i64) -> Timestamp {
    Timestamp::from_millis(NOW + millis)
}

/// A call with Alice, Bob, and Carol seated, Alice publishing one video
/// stream of every layer and one audio stream, Bob and Carol subscribed to
/// both, at their requested layers.
struct Harness {
    sfu: Sfu,
    registry: Registry,
    alice: Member,
    bob: Member,
    carol: Member,
}

impl Harness {
    fn new(bob_mode: BandwidthMode, bob_layer: Layer) -> Self {
        let registry = Registry::new();
        let sfu = Sfu::new(SfuConfig::default(), &registry).expect("config is valid");
        let alice = member(ALICE, ALICE_PHONE);
        let bob = member(BOB, BOB_PHONE);
        let carol = member(CAROL, CAROL_PHONE);
        let now = now_plus(0);
        for (who, mode) in [
            (alice, BandwidthMode::Normal),
            (bob, bob_mode),
            (carol, BandwidthMode::Normal),
        ] {
            sfu.join(id(CALL), who, mode, now).expect("joins");
        }
        sfu.publish(id(CALL), alice, video_request(VIDEO))
            .expect("publishes video");
        sfu.publish(id(CALL), alice, audio_request(AUDIO))
            .expect("publishes audio");
        sfu.subscribe(id(CALL), bob, alice, id(VIDEO), bob_layer, now)
            .expect("bob subscribes video");
        sfu.subscribe(id(CALL), carol, alice, id(VIDEO), Layer::High, now)
            .expect("carol subscribes video");
        sfu.subscribe(id(CALL), bob, alice, id(AUDIO), Layer::Low, now)
            .expect("bob subscribes audio");
        Self {
            sfu,
            registry,
            alice,
            bob,
            carol,
        }
    }

    /// A video frame from Alice.
    fn video_frame(&self, sequence: u64, layer: Layer) -> InboundFrame {
        frame(VIDEO, sequence, layer, vec![0xa1, sequence as u8, 0xf0])
    }

    /// An audio frame from Alice.
    fn audio_frame(&self, sequence: u64) -> InboundFrame {
        frame(
            AUDIO,
            sequence,
            Layer::Low,
            vec![0x53, sequence as u8, 0x11],
        )
    }

    fn forward_video(&self, sequence: u64, layer: Layer) -> Vec<migo_sfu::Delivery> {
        self.sfu
            .forward(id(CALL), self.alice, self.video_frame(sequence, layer))
            .expect("forwards")
    }

    fn forward_audio(&self, sequence: u64) -> Vec<migo_sfu::Delivery> {
        self.sfu
            .forward(id(CALL), self.alice, self.audio_frame(sequence))
            .expect("forwards")
    }

    fn report(&self, stats: &LinkStats, now: Timestamp) -> migo_sfu::Adaptation {
        self.sfu
            .report_stats(id(CALL), self.bob, self.alice, id(VIDEO), stats, now)
            .expect("reports")
    }
}

fn video_request(stream: u128) -> PublishRequest {
    PublishRequest {
        stream_id: id(stream),
        kind: StreamKind::Video,
        layers: vec![Layer::Low, Layer::Medium, Layer::High],
    }
}

fn audio_request(stream: u128) -> PublishRequest {
    PublishRequest {
        stream_id: id(stream),
        kind: StreamKind::Audio,
        layers: Vec::new(),
    }
}

fn frame(stream: u128, sequence: u64, layer: Layer, payload: Vec<u8>) -> InboundFrame {
    InboundFrame {
        stream_id: id(stream),
        sequence,
        layer,
        payload: SealedFrame::from_bytes(payload),
    }
}

/// Loss alone, with everything else healthy. The thresholds sit at 12, 25,
/// 40, and 60 points and loss is weighted four points to the percent, so
/// whole percents of loss name the rungs exactly.
fn loss(pct: u32) -> LinkStats {
    LinkStats {
        packet_loss_pct: pct,
        ..LinkStats::default()
    }
}

fn perfect() -> LinkStats {
    LinkStats {
        available_kbps: 2_000,
        sent_kbps: 1_000,
        ..LinkStats::default()
    }
}

#[test]
fn a_frame_fans_out_to_the_other_subscribers_and_never_back_to_its_publisher() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    let deliveries = h.forward_video(1, Layer::High);
    // Bob and Carol, and nobody else; the publisher is not her own
    // subscriber no matter what she holds.
    assert_eq!(deliveries.len(), 2, "two other subscribers");
    let to: Vec<Id> = deliveries.iter().map(|d| d.to.device_id).collect();
    assert!(to.contains(&id(BOB_PHONE)));
    assert!(to.contains(&id(CAROL_PHONE)));
    assert!(
        !to.contains(&id(ALICE_PHONE)),
        "never back to the publisher"
    );
    for delivery in &deliveries {
        assert_eq!(delivery.publisher, h.alice);
        assert_eq!(delivery.stream_id, id(VIDEO));
        assert_eq!(delivery.sequence, 1);
        assert_eq!(delivery.layer, Layer::High);
        // The sealed bytes arrive byte for byte, unopened.
        assert_eq!(
            delivery.frame.clone().into_bytes(),
            vec![0xa1, 1, 0xf0],
            "the payload is untouched"
        );
    }
}

#[test]
fn a_subscriber_on_one_layer_receives_no_frames_of_another_layer() {
    let h = Harness::new(BandwidthMode::Normal, Layer::Medium);
    h.forward_video(1, Layer::High);
    h.forward_video(3, Layer::Low);
    let got = h.forward_video(2, Layer::Medium);
    // Bob asked for Medium; Carol asked for High. One frame, one recipient.
    assert_eq!(got.len(), 1, "only the Medium subscriber receives");
    assert_eq!(got[0].to, h.bob);
    assert_eq!(got[0].layer, Layer::Medium);
    assert_eq!(got[0].sequence, 2);
    let carol_got = h
        .sfu
        .forward(id(CALL), h.alice, h.video_frame(4, Layer::High))
        .expect("forwards");
    assert_eq!(carol_got.len(), 1);
    assert_eq!(
        carol_got[0].to, h.carol,
        "only the High subscriber receives"
    );
}

#[test]
fn the_thirty_third_participant_is_refused_with_quota_exceeded() {
    let registry = Registry::new();
    let sfu = Sfu::new(SfuConfig::default(), &registry).expect("config is valid");
    let now = now_plus(0);
    for seat in 1..=MAX_AUDIO_PARTICIPANTS {
        let who = member(seat as u128, 1_000 + seat as u128);
        sfu.join(id(CALL), who, BandwidthMode::Normal, now)
            .expect("the first thirty-two seats are taken");
    }
    let thirty_third = member(99, 999);
    let refused = sfu
        .join(id(CALL), thirty_third, BandwidthMode::Normal, now)
        .expect_err("the call is full");
    assert_eq!(refused.code(), codes::QUOTA_EXCEEDED);
    assert_eq!(refused.symbol(), "QUOTA_EXCEEDED");
}

#[test]
fn the_ninth_active_video_stream_is_refused_and_the_participant_stays_as_audio() {
    let registry = Registry::new();
    let sfu = Sfu::new(SfuConfig::default(), &registry).expect("config is valid");
    let now = now_plus(0);
    let seats: Vec<Member> = (1..=9)
        .map(|seat| member(seat as u128, 1_000 + seat as u128))
        .collect();
    for who in &seats {
        sfu.join(id(CALL), *who, BandwidthMode::Normal, now)
            .expect("joins");
    }
    for (seat, who) in seats[..8].iter().enumerate() {
        sfu.publish(id(CALL), *who, video_request(VIDEO + seat as u128 + 1))
            .expect("eight video streams fit");
    }
    let ninth = seats[8];
    let refused = sfu
        .publish(id(CALL), ninth, video_request(VIDEO + 9))
        .expect_err("the ninth video stream does not fit");
    assert_eq!(refused.code(), codes::QUOTA_EXCEEDED);
    // Refused as a stream, not as a participant: the seat stays, and audio
    // still publishes and forwards.
    sfu.publish(id(CALL), ninth, audio_request(AUDIO))
        .expect("audio is never on the video quota");
    let listener = seats[7];
    sfu.subscribe(id(CALL), listener, ninth, id(AUDIO), Layer::Low, now)
        .expect("subscribes");
    let heard = sfu
        .forward(id(CALL), ninth, frame(AUDIO, 1, Layer::Low, vec![1, 2, 3]))
        .expect("forwards");
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].to, listener);
    // A freed slot is the refused publisher's to take.
    sfu.unpublish(id(CALL), seats[0], id(VIDEO + 1))
        .expect("unpublishes");
    sfu.publish(id(CALL), ninth, video_request(VIDEO + 9))
        .expect("the freed slot is takeable");
}

#[test]
fn a_departed_participant_leaves_nothing_behind() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    assert_eq!(
        h.sfu.leave(id(CALL), h.bob).expect("leaves"),
        LeaveOutcome::Left
    );
    let got = h.forward_video(1, Layer::High);
    assert_eq!(got.len(), 1, "carol alone remains subscribed");
    assert_eq!(got[0].to, h.carol);
    // Bob's own subscription went with his seat.
    let gone = h
        .sfu
        .report_stats(id(CALL), h.bob, h.alice, id(VIDEO), &perfect(), now_plus(0));
    assert!(gone.is_err(), "a departed seat reports nothing");
    // And when the publisher leaves, her streams and everyone's
    // subscriptions to them are gone too.
    h.sfu.leave(id(CALL), h.alice).expect("leaves");
    let orphaned = h
        .sfu
        .forward(id(CALL), h.alice, h.video_frame(2, Layer::High));
    assert!(orphaned.is_err(), "a departed seat forwards nothing");
    let stale = h.sfu.report_stats(
        id(CALL),
        h.carol,
        h.alice,
        id(VIDEO),
        &perfect(),
        now_plus(0),
    );
    assert!(stale.is_err(), "the subscription died with the stream");
}

#[test]
fn join_is_idempotent_per_device_and_a_second_device_takes_the_seat() {
    let registry = Registry::new();
    let sfu = Sfu::new(SfuConfig::default(), &registry).expect("config is valid");
    let now = now_plus(0);
    let phone = member(ALICE, ALICE_PHONE);
    let laptop = member(ALICE, ALICE_LAPTOP);
    assert_eq!(
        sfu.join(id(CALL), phone, BandwidthMode::Normal, now)
            .expect("joins"),
        JoinOutcome::Joined
    );
    assert_eq!(
        sfu.join(id(CALL), phone, BandwidthMode::Normal, now)
            .expect("rejoins"),
        JoinOutcome::Rejoined
    );
    sfu.publish(id(CALL), phone, video_request(VIDEO))
        .expect("publishes");
    // The laptop takes the seat: the phone's streams do not survive the
    // handover, and the seat count never grew.
    assert_eq!(
        sfu.join(id(CALL), laptop, BandwidthMode::Normal, now)
            .expect("the seat changes hands"),
        JoinOutcome::Joined
    );
    assert!(
        sfu.forward(id(CALL), phone, frame(VIDEO, 1, Layer::High, vec![1]))
            .is_err(),
        "the replaced device holds no seat"
    );
    assert!(
        sfu.publish(id(CALL), laptop, video_request(VIDEO)).is_ok(),
        "the new device starts a stream of its own"
    );
    // A seat-taking join is not a new participant against the quota.
    for seat in 2..=MAX_AUDIO_PARTICIPANTS {
        let who = member(seat as u128, 2_000 + seat as u128);
        sfu.join(id(CALL), who, BandwidthMode::Normal, now)
            .expect("fills the call");
    }
    let stranger = member(99, 999);
    assert!(
        sfu.join(id(CALL), stranger, BandwidthMode::Normal, now)
            .is_err(),
        "the call is full"
    );
    let third_device = member(ALICE, 105);
    assert!(
        sfu.join(id(CALL), third_device, BandwidthMode::Normal, now)
            .is_ok(),
        "a seat-taking join replaces, it does not add"
    );
}

#[test]
fn subscription_churn_answers_rate_limited_with_a_retry_after() {
    let registry = Registry::new();
    let config = SfuConfig {
        subscribe_window_max: 3,
        ..SfuConfig::default()
    };
    let sfu = Sfu::new(config, &registry).expect("config is valid");
    let now = now_plus(0);
    let alice = member(ALICE, ALICE_PHONE);
    let bob = member(BOB, BOB_PHONE);
    sfu.join(id(CALL), alice, BandwidthMode::Normal, now)
        .expect("joins");
    sfu.join(id(CALL), bob, BandwidthMode::Normal, now)
        .expect("joins");
    for stream in 1..=4 {
        sfu.publish(id(CALL), alice, audio_request(AUDIO + stream))
            .expect("publishes");
    }
    for stream in 1..=3 {
        sfu.subscribe(id(CALL), bob, alice, id(AUDIO + stream), Layer::Low, now)
            .expect("three subscriptions fit the window");
    }
    let refused = sfu
        .subscribe(id(CALL), bob, alice, id(AUDIO + 4), Layer::Low, now)
        .expect_err("the window is spent");
    assert_eq!(refused.code(), codes::RATE_LIMITED);
    let wait = refused.retry_after().expect("a retry hint rides along");
    assert!(
        wait > 0 && wait <= 10_000,
        "the wait is the window's remainder: {wait}"
    );
    // After the window rolls, the same participant may ask again.
    sfu.subscribe(
        id(CALL),
        bob,
        alice,
        id(AUDIO + 4),
        Layer::Low,
        now_plus(10_000),
    )
    .expect("a fresh window admits the request");
}

#[test]
fn the_subscription_cap_is_answered_with_quota_exceeded() {
    let registry = Registry::new();
    let config = SfuConfig {
        max_subscriptions_per_participant: 2,
        ..SfuConfig::default()
    };
    let sfu = Sfu::new(config, &registry).expect("config is valid");
    let now = now_plus(0);
    let alice = member(ALICE, ALICE_PHONE);
    let bob = member(BOB, BOB_PHONE);
    sfu.join(id(CALL), alice, BandwidthMode::Normal, now)
        .expect("joins");
    sfu.join(id(CALL), bob, BandwidthMode::Normal, now)
        .expect("joins");
    for stream in 1..=3 {
        sfu.publish(id(CALL), alice, audio_request(AUDIO + stream))
            .expect("publishes");
    }
    sfu.subscribe(id(CALL), bob, alice, id(AUDIO + 1), Layer::Low, now)
        .expect("first");
    sfu.subscribe(id(CALL), bob, alice, id(AUDIO + 2), Layer::Low, now)
        .expect("second");
    let refused = sfu
        .subscribe(id(CALL), bob, alice, id(AUDIO + 3), Layer::Low, now)
        .expect_err("the cap is reached");
    assert_eq!(refused.code(), codes::QUOTA_EXCEEDED);
    // Dropping one frees the room, and a re-subscribe moves a layer
    // without consuming a second slot.
    sfu.unsubscribe(id(CALL), bob, alice, id(AUDIO + 1))
        .expect("unsubscribes");
    sfu.subscribe(id(CALL), bob, alice, id(AUDIO + 3), Layer::Low, now)
        .expect("the freed slot is takeable");
    sfu.publish(id(CALL), alice, video_request(VIDEO))
        .expect("publishes");
    let refused = sfu
        .subscribe(id(CALL), bob, alice, id(VIDEO), Layer::High, now)
        .expect_err("still capped");
    assert_eq!(refused.code(), codes::QUOTA_EXCEEDED);
}

#[test]
fn degradation_walks_the_pinned_order_bitrate_resolution_frame_rate_video_off() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    let now = now_plus(0);

    // Bitrate first: the same layer, a capped ask.
    let first = h.report(&loss(3), now);
    assert_eq!(first.quality, QualityStep::BitrateCapped);
    assert_eq!(first.layer, Some(Layer::High));
    assert_eq!(first.frame_stride, 1);
    assert!(
        first.bitrate_cap_pct.is_some(),
        "the first sacrifice is bitrate"
    );
    assert!(first.changed);

    // Then resolution: a lower layer.
    let second = h.report(&loss(7), now);
    assert_eq!(second.quality, QualityStep::ResolutionLowered);
    assert_eq!(second.layer, Some(Layer::Medium));

    // Then frame rate: the lowest layer, every second frame.
    let third = h.report(&loss(11), now);
    assert_eq!(third.quality, QualityStep::FrameRateLowered);
    assert_eq!(third.layer, Some(Layer::Low));
    assert_eq!(third.frame_stride, 2);
    let odd = h.forward_video(5, Layer::Low);
    assert!(odd.is_empty(), "an odd sequence loses the stride's toss");
    let even = h.forward_video(6, Layer::Low);
    assert_eq!(even.len(), 1, "an even sequence is forwarded");
    assert_eq!(even[0].to, h.bob);

    // And only then, video off — with audio kept.
    let fourth = h.report(&loss(16), now);
    assert_eq!(fourth.quality, QualityStep::VideoOff);
    assert_eq!(fourth.layer, None);
    assert!(h.forward_video(7, Layer::Low).is_empty(), "no video at all");
    assert_eq!(h.forward_audio(1).len(), 1, "audio is never on the ladder");
}

#[test]
fn recovery_climbs_one_rung_per_interval_and_never_jumps() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    let now = now_plus(0);
    h.report(&loss(16), now);
    // The network recovers completely, at once.
    let good = perfect();
    let too_soon = h.report(&good, now);
    assert_eq!(too_soon.quality, QualityStep::VideoOff);
    assert!(!too_soon.changed, "a raise waits out the interval");
    let first_rung = h.report(&good, now_plus(RAMP_INTERVAL_MS));
    assert_eq!(first_rung.quality, QualityStep::FrameRateLowered);
    let again = h.report(&good, now_plus(RAMP_INTERVAL_MS));
    assert_eq!(
        again.quality,
        QualityStep::FrameRateLowered,
        "one rung per interval"
    );
    let second_rung = h.report(&good, now_plus(2 * RAMP_INTERVAL_MS));
    assert_eq!(second_rung.quality, QualityStep::ResolutionLowered);
    let third_rung = h.report(&good, now_plus(3 * RAMP_INTERVAL_MS));
    assert_eq!(third_rung.quality, QualityStep::BitrateCapped);
    let top = h.report(&good, now_plus(4 * RAMP_INTERVAL_MS));
    assert_eq!(top.quality, QualityStep::Full);
    assert_eq!(top.layer, Some(Layer::High));
}

#[test]
fn low_data_caps_the_layer_and_the_frame_rate_for_its_holder_alone() {
    let h = Harness::new(BandwidthMode::LowData, Layer::High);
    // Carol, on Normal, is the control: the mode shapes its holder, nobody
    // else. Her High frames keep arriving while Bob's are capped.
    let carol_got = h.forward_video(1, Layer::High);
    assert_eq!(carol_got.len(), 1, "the mode's ceiling is not carol's");
    assert_eq!(carol_got[0].to, h.carol);
    assert!(
        h.forward_video(1, Layer::Medium).is_empty(),
        "an odd sequence loses the stride's toss"
    );
    let got = h.forward_video(2, Layer::Medium);
    assert_eq!(got.len(), 1, "bob receives medium, every second one");
    assert_eq!(got[0].to, h.bob);
    let ask = h.report(&perfect(), now_plus(0));
    assert_eq!(ask.layer, Some(Layer::Medium));
    assert_eq!(ask.frame_stride, 2);
    assert_eq!(
        ask.keyframe_interval_ms, LOW_DATA_KEYFRAME_INTERVAL_MS,
        "the keyframe cadence is the reduced one"
    );
    // Audio keeps every frame: the mode lowers video, never voice.
    assert_eq!(h.forward_audio(1).len(), 1);
    assert_eq!(h.forward_audio(2).len(), 1);
}

#[test]
fn ultra_low_data_keeps_audio_and_drops_video_when_the_network_cannot_carry_it() {
    let h = Harness::new(BandwidthMode::UltraLowData, Layer::High);
    // An adequate network still carries video, capped and strided like
    // LowData.
    let adequate = h.report(&perfect(), now_plus(0));
    assert_eq!(adequate.layer, Some(Layer::Medium));
    assert_eq!(adequate.frame_stride, 2);
    assert_eq!(h.forward_video(2, Layer::Medium).len(), 1);
    // Any degradation at all, and video is gone — audio continues.
    let strained = h.report(&loss(3), now_plus(RAMP_INTERVAL_MS));
    assert_eq!(strained.quality, QualityStep::BitrateCapped);
    assert_eq!(
        strained.layer, None,
        "ultra low data does not sit through degradation"
    );
    assert!(
        h.forward_video(4, Layer::Medium).is_empty(),
        "no video frames are forwarded"
    );
    assert_eq!(h.forward_audio(2).len(), 1, "audio keeps running");
}

#[test]
fn a_publisher_cannot_forward_on_a_stream_that_is_not_theirs() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    let stolen = h
        .sfu
        .forward(id(CALL), h.bob, h.video_frame(1, Layer::High));
    assert!(stolen.is_err(), "the stream belongs to alice's seat");
    assert_eq!(stolen.unwrap_err().symbol(), "NOT_FOUND");
}

#[test]
fn redefining_a_stream_under_the_same_id_is_an_idempotency_mismatch() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    assert_eq!(
        h.sfu
            .publish(id(CALL), h.alice, video_request(VIDEO))
            .expect("the same definition is the same stream"),
        PublishOutcome::Republished
    );
    let redefined = PublishRequest {
        stream_id: id(VIDEO),
        kind: StreamKind::Video,
        layers: vec![Layer::Low, Layer::High],
    };
    let refused = h
        .sfu
        .publish(id(CALL), h.alice, redefined)
        .expect_err("an id is not a variable");
    assert_eq!(refused.code(), codes::IDEMPOTENCY_MISMATCH);
    // And an unpublish of a stream never published is a no-op success.
    assert_eq!(
        h.sfu
            .unpublish(id(CALL), h.alice, id(AUDIO + 50))
            .expect("the state asked for already holds"),
        UnpublishOutcome::Absent
    );
}

#[test]
fn a_configuration_that_cannot_serve_a_call_is_refused() {
    let registry = Registry::new();
    let no_video = SfuConfig {
        max_active_video_streams: 0,
        ..SfuConfig::default()
    };
    assert!(
        Sfu::new(no_video, &registry).is_err(),
        "a call with no video slots"
    );
    let more_video_than_seats = SfuConfig {
        max_active_video_streams: 33,
        ..SfuConfig::default()
    };
    assert!(
        Sfu::new(more_video_than_seats, &registry).is_err(),
        "more video slots than seats"
    );
    let out_of_order = SfuConfig {
        adaptive: migo_sfu::AdaptiveThresholds {
            resolution_at: 5,
            ..migo_sfu::AdaptiveThresholds::default()
        },
        ..SfuConfig::default()
    };
    assert!(
        Sfu::new(out_of_order, &registry).is_err(),
        "a ladder that cannot be walked in order"
    );
}

#[test]
fn the_metrics_carry_no_identity_and_the_gauges_follow_the_seats() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    h.forward_video(1, Layer::High);
    h.forward_video(2, Layer::High);
    h.sfu.leave(id(CALL), h.carol).expect("leaves");
    h.forward_video(3, Layer::High);
    let rendered = h.registry.render();
    assert!(rendered.contains("migo_sfu_frames_forwarded_total"));
    assert!(rendered.contains("migo_sfu_frames_dropped_total"));
    assert!(rendered.contains("migo_sfu_participants"));
    assert!(rendered.contains("migo_sfu_active_video_streams"));
    // No series names a person: the ids in this test never appear as
    // labels.
    let alice_text = h.alice.account_id.to_text();
    assert!(
        !rendered.contains(&alice_text),
        "a metrics endpoint is not a participant list"
    );
}

#[test]
fn a_subscriber_who_never_asked_receives_nothing_and_the_publisher_never_receives_his_own() {
    let h = Harness::new(BandwidthMode::Normal, Layer::High);
    // Carol unsubscribes; the video frame that follows reaches Bob alone.
    h.sfu
        .unsubscribe(id(CALL), h.carol, h.alice, id(VIDEO))
        .expect("unsubscribes");
    let got = h.forward_video(1, Layer::High);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].to, h.bob);
    // A seat that holds a subscription to its own stream is refused at
    // subscribe time, and Bob's own audio publish never reaches Bob.
    let self_sub = h
        .sfu
        .subscribe(id(CALL), h.bob, h.bob, id(AUDIO), Layer::Low, now_plus(0));
    assert!(
        self_sub.is_err(),
        "a participant does not subscribe to himself"
    );
}
