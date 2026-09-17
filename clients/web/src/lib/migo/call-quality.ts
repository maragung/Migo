'use client';

/**
 * The 1:1 call's quality plane: section 180's five tiers, measured on the one link a two-party call
 * has, and acted on by the same ladder a group call runs.
 *
 * # One ladder, two planes
 *
 * The model — congestion score, the rungs, the thresholds, the ramp — lives in
 * {@link ./group-media.ts}, because the group plane needed all of it first, and this module imports
 * it rather than restating it. That direction matters: two copies of a ladder is two ladders, and
 * the day one of them changes the two clients that share this code would disagree about what a
 * congested call does. What is *not* shared is what a rung is applied to — a group call has one
 * sender per link and a 1:1 call has one sender in total — so {@link shapeVideoSender} takes the
 * sender as an argument and both planes call the same function with their own.
 *
 * # What is measured, and what the measurement is worth
 *
 * Loss, RTT, jitter, sent bitrate, dropped frames and — where the browser reports it — an outgoing
 * bandwidth estimate, all as deltas between two readings of one connection. A delta and not a
 * lifetime total, because a call that started healthy and degraded later has a lifetime total that
 * says nothing about now. The first reading of a link has nothing to subtract, so it produces no
 * judgement at all: {@link advanceMeasurement} answers `null` rather than guessing a rung from a
 * sample it cannot compare.
 *
 * # The five tiers are section 180's words, not the ladder's
 *
 * The ladder's rung names say what is being given up (`bitrate-capped`, `video-off`); section 180
 * names what the user is shown (Excellent, Good, Average, Poor, Very poor). Both are kept, mapped
 * in one place here, because a user reading "resolution-lowered" has been told about the
 * implementation rather than about their call.
 *
 * Section 180 also attaches a resolution to each tier — 1080p down to audio-only — and this module
 * deliberately does not claim one. The rungs are relative sacrifices (halve the resolution, halve
 * the frame rate), not absolute targets: the top rung sends whatever the camera gives, which on
 * this client is whatever `getUserMedia` returned, and a screen reading "1080p" over a 480p webcam
 * would be reporting a number nothing measured. The tier is a network judgement, which is what an
 * indicator of network quality should be; meeting the resolution half of section 180 means
 * constraining the capture, which is a separate piece of work.
 *
 * # A voice call is measured too
 *
 * The indicator is not a video feature. A voice call on a lossy link has a worse score and the user
 * should see that before they conclude the other person has stopped talking. So the tier is shown
 * for both kinds of call, while the *Degraded* state — which section 180 defines as connected with
 * video paused — is only reachable on a video call, because on a voice call there is no video to
 * pause and claiming the state would be describing a sacrifice nobody made.
 */

import type { CallStats } from '@migo/sdk';

import {
  DEFAULT_ADAPTIVE_THRESHOLDS,
  RAMP_INTERVAL_MS,
  advanceQuality,
  linkStatsBetween,
  targetQuality,
} from './group-media.js';
import type { AdaptiveThresholds, LinkQuality, LinkStats, RawLinkCounters } from './group-media.js';

/**
 * How often a connected call samples its link. Two seconds is the coarsest interval that still
 * notices a collapse inside the reconnect window, and it is the group plane's own cadence, so both
 * planes step the ladder at the same rate.
 */
export const QUALITY_POLL_MS = 2_000;

/**
 * Section 180's word for each rung. The ladder has five rungs and the requirement has five tiers,
 * in the same order, so this is a rename rather than a second classification — and the order is
 * pinned by {@link QUALITY_LADDER}, not by this object's key order.
 */
const TIER_LABELS: Record<LinkQuality, string> = {
  full: 'Excellent',
  'bitrate-capped': 'Good',
  'resolution-lowered': 'Average',
  'frame-rate-lowered': 'Poor',
  'video-off': 'Very poor',
};

/** The word a user reads for a rung. */
export function qualityTierLabel(quality: LinkQuality): string {
  return TIER_LABELS[quality];
}

/**
 * Whether a rung is the *Degraded* screen state — connected, with video paused because the link
 * could not carry it.
 *
 * Only a video call can be degraded: the bottom rung means this endpoint stopped sending its own
 * camera, which a voice call was never doing, and on a voice call the same rung is a network fact
 * the indicator reports without the call having given anything up.
 */
export function degradedAt(quality: LinkQuality, isVideo: boolean): boolean {
  return isVideo && quality === 'video-off';
}

/** One step of the ladder, as a measurement produced it. */
export interface QualityAdvance {
  /** The rung the link is on after this step. */
  quality: LinkQuality;
  /** The stretch's measurements, which produced the rung. */
  stats: LinkStats;
  /** Whether the rung moved, so only a move needs acting on or reporting. */
  changed: boolean;
}

/**
 * Steps the ladder once, from a fresh reading and the reading before it.
 *
 * `null` when there is nothing to step with — the first reading of a link, a stretch too short to
 * have a rate, or a connection that reported nothing usable. `changedAtMs` is when the rung last
 * moved, and it is what the recovery ramp is measured from: the caller owns it, because only the
 * caller knows whether it just applied a change.
 */
export function advanceMeasurement(
  current: LinkQuality,
  previous: RawLinkCounters | null,
  counters: RawLinkCounters,
  changedAtMs: number,
  nowMs: number,
  thresholds: AdaptiveThresholds = DEFAULT_ADAPTIVE_THRESHOLDS,
  rampIntervalMs: number = RAMP_INTERVAL_MS,
): QualityAdvance | null {
  if (previous === null) {
    return null;
  }
  const stats = linkStatsBetween(previous, counters);
  if (stats === null) {
    return null;
  }
  const quality = advanceQuality(
    current,
    targetQuality(stats, thresholds),
    changedAtMs,
    nowMs,
    rampIntervalMs,
  );
  return { quality, stats, changed: quality !== current };
}

/**
 * The numbers a measurement reports to the server, in the wire's own units.
 *
 * Only what the client actually measured: RTT, loss, jitter, and whether the media is riding a
 * relay. Section 180's remaining metrics — node, SFU load — are the server's to know, and the
 * client inventing them would be inventing the numbers capacity decisions are made from.
 *
 * `packetLoss` is per-10000 on the wire while the model carries whole percents, so the conversion
 * happens here, at the boundary, where the unit change is visible to a reader.
 */
export function qualityReport(
  stats: LinkStats,
  counters: RawLinkCounters,
): Partial<Omit<CallStats, 'callId'>> {
  return {
    rttMs: stats.rttMs,
    packetLoss: Math.max(0, Math.round(stats.packetLossPct * 100)),
    jitterMs: stats.jitterMs,
    // `undefined` when the connection has named no nominated pair yet, which is not the same fact
    // as "not relaying" and must not be reported as one.
    ...(counters.relay === undefined ? {} : { usedTurn: counters.relay }),
  };
}
