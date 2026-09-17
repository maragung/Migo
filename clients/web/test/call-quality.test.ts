/**
 * The 1:1 quality plane's arithmetic, pinned without a peer connection.
 *
 * Three things here can regress silently, and each is pinned against the way it would:
 *
 *   1. **The first reading judges nothing.** A single sample has no stretch to measure, and a rung
 *      stepped from one is a rung stepped from noise. The `null` is the test.
 *   2. **Bad news travels instantly, good news climbs.** The ladder exists to stop a call
 *      oscillating, so a collapse must reach its rung at once while a recovery gives back one rung
 *      per ramp interval — measured from the last *move*, not from the last sample.
 *   3. **A rung does the same thing in both planes.** {@link shapeVideoSender} is the group plane's
 *      function, called here on a 1:1 call's single sender, so what it does is not a detail of
 *      either plane; what is pinned is that a grounded sender lets go of the track rather than
 *      muting it, that a camera that is off is never re-attached by a healthy link, and that the
 *      caps are a share of what the link is measured to be sending rather than fixed numbers.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  LOW_BANDWIDTH_CEILING,
  QUALITY_CEILINGS,
  QUALITY_POLL_MS,
  advanceMeasurement,
  cappedQuality,
  degradedAt,
  qualityReport,
  qualityTierLabel,
} from '../src/lib/migo/call-quality.js';
import {
  LOW_BANDWIDTH_AUDIO_BPS,
  NO_CAP_FRAMERATE,
  NO_CAP_KBPS,
  NO_CAP_SCALE,
  QUALITY_LADDER,
  RAMP_INTERVAL_MS,
  shapeAudioSender,
  shapeVideoSender,
  videoSenderParams,
} from '../src/lib/migo/group-media.js';
import type { LinkQuality, RawLinkCounters } from '../src/lib/migo/group-media.js';

const T0 = 1_700_000_000_000;
const POLL = QUALITY_POLL_MS;

/** One poll's worth of traffic, as the counters advance by it. */
interface Interval {
  received: number;
  lost: number;
  frames: number;
  dropped: number;
  bytes: number;
  rttMs: number;
  jitterMs: number;
  availableKbps: number | null;
}

/** A healthy poll: everything arrives, nothing is late, and the link carries more than is sent. */
const HEALTHY: Interval = {
  received: 10_000,
  lost: 0,
  frames: 3_000,
  dropped: 0,
  bytes: 250_000,
  rttMs: 20,
  jitterMs: 2,
  availableKbps: 4_000,
};

/** A collapsed poll: nine tenths lost, nine tenths of the frames dropped, and 900 ms late. */
const COLLAPSED: Interval = {
  received: 1_000,
  lost: 9_000,
  frames: 300,
  dropped: 2_700,
  bytes: 100_000,
  rttMs: 900,
  jitterMs: 120,
  availableKbps: 200,
};

/** The counters a connection reports before any media has flowed. */
const IDLE: RawLinkCounters = {
  at: T0,
  packetsLost: 0,
  packetsReceived: 0,
  framesReceived: 0,
  framesDropped: 0,
  bytesSent: 0,
  rttMs: 0,
  jitterMs: 0,
  availableKbps: null,
};

/**
 * The reading one poll later: the counters so far carried forward by one interval of traffic.
 *
 * `linkStatsBetween` reads deltas, so a fixture that repeated a total would measure nothing at all —
 * which is why every reading here is built from the one before it rather than written out whole.
 * RTT, jitter and the bandwidth estimate are instantaneous, so those come from the new interval
 * alone, exactly as {@link readLinkCounters} reports them.
 */
function after(previous: RawLinkCounters, interval: Interval, at: number): RawLinkCounters {
  return {
    at,
    packetsLost: previous.packetsLost + interval.lost,
    packetsReceived: previous.packetsReceived + interval.received,
    framesReceived: previous.framesReceived + interval.frames,
    framesDropped: previous.framesDropped + interval.dropped,
    bytesSent: previous.bytesSent + interval.bytes,
    rttMs: interval.rttMs,
    jitterMs: interval.jitterMs,
    availableKbps: interval.availableKbps,
  };
}

test('the first reading of a link produces no judgement at all', () => {
  // One sample is not a measurement, and a rung guessed from it would degrade or spare a call on no
  // evidence. The manager's first poll only sets the baseline this is the null for.
  const healthy = after(IDLE, HEALTHY, T0 + POLL);
  assert.equal(advanceMeasurement('full', null, healthy, T0, T0 + POLL), null);

  // The second reading is the first one with anything to say, and a collapse says all of it: the
  // ladder's order is the order of what is given up, so arriving at the bottom costs no more than
  // arriving at the rung above it.
  const collapsed = after(healthy, COLLAPSED, T0 + 2 * POLL);
  const step = advanceMeasurement('full', healthy, collapsed, T0 + POLL, T0 + 2 * POLL);
  assert.ok(step !== null, 'the second reading is where the ladder gets to speak');
  assert.equal(step.changed, true);
  assert.equal(step.quality, 'video-off', 'a collapse reaches the bottom rung in one step');
  assert.ok(step.stats.packetLossPct > 50, 'the loss the rung was read from is reported');
});

test('a steady link keeps the rung it is on, and a stretch with no time in it says nothing', () => {
  const first = after(IDLE, HEALTHY, T0 + POLL);
  const second = after(first, HEALTHY, T0 + 2 * POLL);
  const step = advanceMeasurement('full', first, second, T0 + POLL, T0 + 2 * POLL);
  assert.ok(step !== null);
  assert.equal(step.quality, 'full');
  assert.equal(step.changed, false, 'a healthy link reports the rung it was already on');

  // Two readings of the same instant divide by zero, and a rate with no time under it is not a rate.
  assert.equal(
    advanceMeasurement('full', first, after(first, HEALTHY, T0 + POLL), T0 + POLL, T0 + POLL),
    null,
  );
  // A stretch in which nothing arrived and nothing was lost has no denominator either, so a link
  // that has gone quiet is not a link that has gone bad.
  const silent = after(first, { ...HEALTHY, received: 0, frames: 0 }, T0 + 2 * POLL);
  assert.equal(advanceMeasurement('full', first, silent, T0 + POLL, T0 + 2 * POLL), null);
});

test('recovery climbs one rung per ramp interval, and never in a jump', () => {
  // The link is healthy again in the very next sample, but the ladder moved a moment ago. Climbing
  // straight back to the top is the oscillation the ramp exists to prevent.
  const collapsed = after(IDLE, COLLAPSED, T0 + POLL);
  const justBefore = T0 + POLL + RAMP_INTERVAL_MS - 1;
  const early = advanceMeasurement(
    'video-off',
    collapsed,
    after(collapsed, HEALTHY, justBefore),
    T0 + POLL,
    justBefore,
  );
  assert.ok(early !== null);
  assert.equal(early.quality, 'video-off', 'the ramp has not elapsed, so the rung holds');
  assert.equal(early.changed, false);

  // One interval after the *move* — not after the last sample — it gives back exactly one rung.
  const movedAt = T0 + POLL;
  const later = movedAt + RAMP_INTERVAL_MS;
  const before = after(collapsed, HEALTHY, later - POLL);
  const climbed = advanceMeasurement(
    'video-off',
    before,
    after(before, HEALTHY, later),
    movedAt,
    later,
  );
  assert.ok(climbed !== null);
  assert.equal(climbed.quality, 'frame-rate-lowered', 'one rung, and only one');
  assert.equal(climbed.changed, true);
});

test('the whole ladder is given up in one step and climbed back a rung at a time', () => {
  const healthy = after(IDLE, HEALTHY, T0 + POLL);
  const bottom = QUALITY_LADDER[QUALITY_LADDER.length - 1];
  assert.equal(bottom, 'video-off', 'the ladder ends at the rung with no video');
  const worst = advanceMeasurement(
    'full',
    healthy,
    after(healthy, COLLAPSED, T0 + 2 * POLL),
    T0 + POLL,
    T0 + 2 * POLL,
  );
  assert.ok(worst !== null);
  assert.equal(worst.quality, bottom);

  // Up: one step per ramp interval, each giving back one rung, so the climb visits every rung of the
  // ladder in order and arrives at the top after one step for each rung below it.
  let rung: LinkQuality = worst.quality;
  let counters = after(healthy, COLLAPSED, T0 + 2 * POLL);
  let movedAt = T0 + 2 * POLL;
  const seen: LinkQuality[] = [rung];
  for (let step = 1; step < QUALITY_LADDER.length; step += 1) {
    const at = movedAt + RAMP_INTERVAL_MS;
    const before = after(counters, HEALTHY, at - POLL);
    counters = after(before, HEALTHY, at);
    const advanced = advanceMeasurement(rung, before, counters, movedAt, at);
    assert.ok(advanced !== null, `step ${step} has a stretch to read`);
    assert.equal(advanced.changed, true, `step ${step} moves, or the climb has stalled`);
    rung = advanced.quality;
    seen.push(rung);
    movedAt = at;
  }
  assert.deepEqual(seen, [...QUALITY_LADDER].reverse(), 'the climb visits every rung in order');
  assert.equal(rung, 'full');
});

test('only a video call can be degraded, and only on the bottom rung', () => {
  assert.equal(degradedAt('video-off', true), true);
  assert.equal(degradedAt('video-off', false), false, 'a voice call has no video to have paused');
  for (const rung of QUALITY_LADDER.slice(0, -1)) {
    assert.equal(degradedAt(rung, true), false, `${rung} still has video`);
  }
});

test('every rung has one of section 180’s words, and no two rungs share one', () => {
  // The ladder's rung names say what is given up; the requirement names what the user is shown. Both
  // are kept, and this is the single place they are joined.
  const words = QUALITY_LADDER.map((rung) => qualityTierLabel(rung));
  assert.deepEqual(words, ['Excellent', 'Good', 'Average', 'Poor', 'Very poor']);
  assert.equal(new Set(words).size, QUALITY_LADDER.length, 'no two rungs read the same');
});

test('the report carries the measured numbers in the wire’s own units', () => {
  const healthy = after(IDLE, HEALTHY, T0 + POLL);
  const collapsed = after(healthy, COLLAPSED, T0 + 2 * POLL);
  const step = advanceMeasurement('full', healthy, collapsed, T0 + POLL, T0 + 2 * POLL);
  assert.ok(step !== null);

  const report = qualityReport(step.stats, { ...collapsed, relay: true });
  assert.equal(report.rttMs, step.stats.rttMs);
  assert.equal(report.jitterMs, step.stats.jitterMs);
  // Per-10000 on the wire, whole percents in the model: the conversion happens at the boundary,
  // which is the only place a reader can see that the two are different units.
  assert.equal(report.packetLoss, step.stats.packetLossPct * 100);
  assert.equal(report.usedTurn, true);
  // And nothing the client did not measure: the node's own numbers are the node's to report.
  assert.deepEqual(Object.keys(report).sort(), ['jitterMs', 'packetLoss', 'rttMs', 'usedTurn']);
});

test('a connection that has named no candidate pair does not report that it is not relaying', () => {
  // `undefined` is "not known yet" and `false` is "measured, and it is direct". Reporting the first
  // as the second would put calls in the direct column before anything had looked.
  const healthy = after(IDLE, HEALTHY, T0 + POLL);
  const step = advanceMeasurement(
    'full',
    healthy,
    after(healthy, HEALTHY, T0 + 2 * POLL),
    T0 + POLL,
    T0 + 2 * POLL,
  );
  assert.ok(step !== null);
  assert.ok(
    !('usedTurn' in qualityReport(step.stats, healthy)),
    'an unmeasured pair reports no TURN fact at all',
  );
  assert.equal(
    qualityReport(step.stats, { ...healthy, relay: false }).usedTurn,
    false,
    'a measured direct pair says so',
  );
});

// --- the sender, which is the only part of this a peer connection can see ------------------------

/** A stand-in sender: what it carries, what it was last told to send, and how often it was told. */
interface FakeSender {
  sender: RTCRtpSender;
  attached: MediaStreamTrack | null;
  params: RTCRtpSendParameters;
  writes: number;
}

/** The encodings of a fake sender, as the caps are read back off them. */
interface Caps {
  maxBitrate?: number;
  scaleResolutionDownBy?: number;
  maxFramerate?: number;
}

function encodingOf(fake: FakeSender): Caps {
  // No cast: an `RTCRtpEncodingParameters` already satisfies `Caps`, which is the point of
  // declaring `Caps` as the three fields this file reads rather than the browser's whole dictionary.
  return fake.params.encodings[0] ?? {};
}

function fakeSender(): FakeSender {
  const state = {
    sender: null as unknown as RTCRtpSender,
    attached: null as MediaStreamTrack | null,
    params: { encodings: [{}] } as unknown as RTCRtpSendParameters,
    writes: 0,
  };
  state.sender = {
    replaceTrack: (next: MediaStreamTrack | null): Promise<void> => {
      state.attached = next;
      return Promise.resolve();
    },
    getParameters: (): RTCRtpSendParameters => state.params,
    setParameters: (next: RTCRtpSendParameters): Promise<void> => {
      state.params = next;
      state.writes += 1;
      return Promise.resolve();
    },
  } as unknown as RTCRtpSender;
  return state;
}

const CAMERA = { kind: 'video', id: 'cam' } as unknown as MediaStreamTrack;

test('a grounded sender lets go of the camera, and a rung that comes back takes it again', () => {
  const fake = fakeSender();
  // The bottom rung takes the track off the wire rather than muting it: a muted track still costs
  // the encoder, and the same camera has to keep flowing to every link the ladder has not grounded.
  assert.equal(shapeVideoSender(fake.sender, CAMERA, 'video-off', 800, true), false);
  assert.equal(fake.attached, null, 'the track is off the sender');
  assert.equal(fake.writes, 0, 'and there was nothing left to cap');

  assert.equal(shapeVideoSender(fake.sender, CAMERA, 'full', 800, false), true);
  assert.equal(fake.attached, CAMERA, 'a recovered rung puts the camera back');
});

test('a camera that is off stays off however good the link is', () => {
  // The rung decides how much video to send; the user decides whether to send any. A healthy link
  // must not switch a camera back on behind the user's back.
  const fake = fakeSender();
  assert.equal(shapeVideoSender(fake.sender, null, 'full', 800, true), false);
  assert.equal(fake.attached, null, 'nothing is attached when there is no camera to attach');
});

test('a rung’s caps ride the sender’s parameters, and the bottom rung writes none', () => {
  for (const rung of QUALITY_LADDER) {
    const fake = fakeSender();
    const carrying = shapeVideoSender(fake.sender, CAMERA, rung, 1_000, true);
    const expected = videoSenderParams(rung, 1_000);
    if (!expected.enabled) {
      assert.equal(carrying, false, `${rung} carries nothing`);
      assert.equal(fake.attached, null, `${rung} takes the track off`);
      assert.equal(fake.writes, 0, `${rung} has no caps to write`);
      continue;
    }
    assert.equal(carrying, true, `${rung} carries the camera`);
    const encoding = encodingOf(fake);
    assert.equal(
      encoding.maxBitrate,
      expected.maxBitrate * 1000,
      `${rung} bitrate, in the bits per second the sender's parameters take`,
    );
    assert.equal(encoding.scaleResolutionDownBy, expected.scaleResolutionDownBy, `${rung} scale`);
    assert.equal(encoding.maxFramerate, expected.maxFramerate, `${rung} frame rate`);
    assert.equal(fake.writes, 1, `${rung} is written once`);
  }
});

test('a bitrate cap is a share of what the link is measured to be sending', () => {
  // The cap follows the measurement rather than being an absolute number, which is what makes it a
  // share: the same rung on a link sending a quarter as much asks for a quarter as much.
  const slow = fakeSender();
  const fast = fakeSender();
  shapeVideoSender(slow.sender, CAMERA, 'bitrate-capped', 500, true);
  shapeVideoSender(fast.sender, CAMERA, 'bitrate-capped', 2_000, true);
  const slowCap = encodingOf(slow).maxBitrate ?? 0;
  const fastCap = encodingOf(fast).maxBitrate ?? 0;
  assert.equal(fastCap, 4 * slowCap);

  // And a link measured near zero still asks for the floor rather than for nothing, because a rung
  // that capped the bitrate to zero would be the bottom rung wearing another rung's name.
  const idle = fakeSender();
  shapeVideoSender(idle.sender, CAMERA, 'bitrate-capped', 10, true);
  assert.ok((encodingOf(idle).maxBitrate ?? 0) > 0);
});

test('a rung climbed back to gives back every cap the rungs below wrote', () => {
  // A cap cannot be lifted by leaving its member out of the parameters — the sender keeps whatever
  // a member it is not told about already held — so the top rung writes the neutral values over
  // them. Without that, a call that recovers, or a user who lifts the ceiling they pinned, would go
  // on sending the small video the screen no longer claims.
  const fake = fakeSender();
  shapeVideoSender(fake.sender, CAMERA, 'frame-rate-lowered', 1_000, true);
  const capped = encodingOf(fake);
  assert.equal(capped.scaleResolutionDownBy, 2);
  assert.equal(capped.maxFramerate, 15);
  assert.ok((capped.maxBitrate ?? 0) > 0);

  shapeVideoSender(fake.sender, CAMERA, 'full', 1_000, true);
  const lifted = encodingOf(fake);
  assert.equal(lifted.scaleResolutionDownBy, NO_CAP_SCALE, 'the scale is given back as no scaling');
  assert.equal(lifted.maxFramerate, NO_CAP_FRAMERATE);
  assert.equal(lifted.maxBitrate, NO_CAP_KBPS * 1000, 'and the bitrate as the ceiling above it');
  assert.ok(
    (lifted.maxBitrate ?? 0) > (capped.maxBitrate ?? 0),
    'the ceiling it replaced is the larger of the two',
  );

  // And the middle of the ladder is not the top of it: climbing one rung gives back that rung's
  // sacrifice and keeps the rest, which is the difference between a ladder and a switch.
  const half = fakeSender();
  shapeVideoSender(half.sender, CAMERA, 'resolution-lowered', 1_000, true);
  shapeVideoSender(half.sender, CAMERA, 'bitrate-capped', 1_000, true);
  const middle = encodingOf(half);
  assert.equal(middle.scaleResolutionDownBy, NO_CAP_SCALE, 'the resolution is given back');
  assert.ok(
    (middle.maxBitrate ?? 0) < NO_CAP_KBPS * 1000,
    'the bitrate cap it still sits under is not',
  );
});

test('the low-bandwidth mode caps the audio a voice call has instead of video to give up', () => {
  // Audio is never a rung of the ladder — the bottom of it is "video off, audio alive" — so on a
  // voice call, where the ladder has nothing to act on, this cap is the whole of the mode.
  const fake = fakeSender();
  shapeAudioSender(fake.sender, true);
  assert.equal(encodingOf(fake).maxBitrate, LOW_BANDWIDTH_AUDIO_BPS);

  // Turning the mode off lifts the cap rather than leaving the call narrowband for the rest of its
  // life, and it lifts it by writing the ceiling over it: a member left out would be a cap the
  // sender keeps, which is the whole reason this is a number and not a deletion.
  shapeAudioSender(fake.sender, false);
  assert.equal(encodingOf(fake).maxBitrate, NO_CAP_KBPS * 1000);
});

test('a manual tier is a ceiling the link can still descend below', () => {
  // Section 180 asks for manual selection beside the automatic one, and manual is a ceiling and not
  // a floor: no control can make a link carry more than it can, so a call pinned to Good on a link
  // that collapses still gives up video — and the indicator says so, because the tier a user reads
  // is the tier the call is actually on.
  const pinned = 'bitrate-capped';
  assert.equal(
    cappedQuality('full', pinned, false),
    pinned,
    'the ceiling holds a healthy link down',
  );
  assert.equal(
    cappedQuality('resolution-lowered', pinned, false),
    'resolution-lowered',
    'and a link below it is left where it is',
  );
  assert.equal(cappedQuality('video-off', pinned, false), 'video-off');
  assert.equal(cappedQuality('full', null, false), 'full', 'automatic is no ceiling at all');

  // Pinning the top rung is not the same as automatic only in what it says: both leave the call at
  // the top, which is why the menu offers every rung the ladder has and lets the user mean it.
  assert.equal(cappedQuality('full', 'full', false), 'full');

  // The bottom rung is deliberately not offered: it would put a video call into Degraded, a state
  // whose whole meaning is that quality dropped until video was paused, and a sacrifice the user
  // chose is not a drop. A user who wants no video has the camera button.
  assert.deepEqual(QUALITY_CEILINGS, [
    'full',
    'bitrate-capped',
    'resolution-lowered',
    'frame-rate-lowered',
  ]);
  const lowestOffered = QUALITY_CEILINGS.at(-1);
  assert.ok(lowestOffered !== undefined, 'the menu offers something');
  assert.equal(degradedAt(cappedQuality('full', lowestOffered, false), true), false);
});

test('the low-bandwidth mode is a second ceiling, so turning it off gives back what it took', () => {
  // Two controls write to one rung, and neither overwrites the other: the mode is applied as its own
  // ceiling rather than as a rung of its own, so a call the user pinned to Poor and then put in low
  // bandwidth mode is on whichever of the two is lower, and leaving the mode restores the pin.
  assert.equal(cappedQuality('full', null, true), LOW_BANDWIDTH_CEILING);
  assert.equal(cappedQuality('full', 'frame-rate-lowered', true), 'frame-rate-lowered');
  assert.equal(
    cappedQuality('video-off', 'full', true),
    'video-off',
    'and the worse of the link and the controls always wins',
  );
  assert.equal(cappedQuality('full', 'bitrate-capped', true), LOW_BANDWIDTH_CEILING);
  assert.equal(
    cappedQuality('full', 'bitrate-capped', false),
    'bitrate-capped',
    'the pin survives',
  );

  // The mode keeps video: turning it off entirely would be the camera button's job, and a mode that
  // did it would show the user a Degraded call they had asked for.
  assert.equal(degradedAt(LOW_BANDWIDTH_CEILING, true), false);
});
