'use client';

/**
 * The multi-participant media plane of a group call: a mesh of WebRTC peer connections, one per
 * seated device, dialed and answered over the sealed roster relay.
 *
 * # Why a mesh, when the brief draws an SFU
 *
 * Section 166's group-call diagram shows an SFU, and the server's `migo-sfu` crate is its decision
 * core — registry, sealed-frame forwarding, simulcast selection, the quality ladder. But that core
 * has no transport by design ("frame media tidak pernah menaiki MWP"), and no SFU process exists on
 * a node: the server's built group-call surface is *in-message* forwarding between roster devices.
 * So the plane this module builds is the one the built server can actually carry: **direct WebRTC
 * between the roster's devices** — the SFU's forwarding role distributed as a full mesh — with SDP
 * and ICE relayed through the existing sealed `CALL_SDP` / `CALL_ICE` frames. Media crosses the
 * server only where P2P fails, as TURN relay, and section 166's TURN rule is what the mesh keeps:
 * DTLS-SRTP encrypts every link end-to-end, so a relay (TURN or otherwise) forwards ciphertext it
 * cannot open — the same property an SFU with the frame key withheld would have.
 *
 * The SFU decision core still decides here, on the client: its product limits and quality ladder
 * are mirrored below (`MAX_ACTIVE_VIDEO_STREAMS`, the degradation order, the one-rung-per-interval
 * recovery), because those rules belong to the call, not to whichever process happens to enforce
 * them. What a mesh cannot mirror — central simulcast selection and per-subscriber layer switching
 * on someone else's uplink — stays honestly unmirrored; each link shapes only what its *own*
 * endpoint sends.
 *
 * # What is sealed, and under which key
 *
 * Every SDP description and ICE batch this plane exchanges is sealed under the call's **frame key**
 * — the shared key the SDK's `GroupCallKeysDomain` mints, distributes, and rotates (section 163).
 * That is the one key the whole roster holds, it is distributed device-to-device under sealed
 * envelopes the server never opens, and it rotates exactly when membership changes — so a leaver
 * cannot read the signaling that follows their departure and a joiner cannot read what predates
 * their arrival. The frame key is also the AEAD's binding: the epoch is associated data, so a
 * rotation invalidates the sealing of anything still mid-negotiation — which is why a key change
 * ({@link GroupMediaPlane.keyEpochChanged}) resets every link that has not connected yet, and the
 * dialer re-dials under the fresh key.
 *
 * The join frame's own `sealedOffer` slot still carries the roster module's placeholder — honestly,
 * because it must: the joiner holds no frame key at the moment of joining (the key arrives with the
 * roster snapshot, or with a distributor's answer), so nothing the joiner seals in the join frame
 * could be opened by anyone else. The real media descriptions ride per-peer offers on the relay,
 * where both ends already hold the key.
 *
 * # Who dials whom
 *
 * Every client holds the same join-order projection (the snapshot's verbatim order, announcements
 * folded in arrival order), and the rule is deterministic over it: **the seat that joined later
 * dials; the seat that joined earlier answers.** A mid-call joiner is therefore always the offerer
 * toward everyone seated, which is also the ordering the key machinery produces — the joiner
 * completes its key ask *before* it can seal an offer, so an offer can never precede its key. A
 * seat replacement is a departure followed by an arrival at the end of the join order, so the
 * replacing device re-dials everyone, exactly as a fresh joiner does. When two joins cross and both
 * devices briefly believe they are the later seat, the glare is broken by device id: the
 * lexicographically smaller id keeps its offer, the other rolls back and answers.
 *
 * # The quality ladder, mirrored from `migo-sfu`
 *
 * The ladder is section 165's pinned order — bitrate first, then resolution, then frame rate, and
 * only then video, with audio never on the ladder. Degradation lands on the target rung at once (a
 * saturated link helps nobody), recovery climbs one rung per interval, never a jump. The classifier
 * is the same arithmetic over the same six reported numbers, with the same default thresholds; what
 * a browser cannot measure honestly is mapped where it can be and nowhere else (see {@link
 * linkStatsBetween}). Shaping applies to this endpoint's own video sender on the affected link:
 * `setParameters` for the caps the sender can honor, `replaceTrack(null)` for the rung that turns
 * video off per-link — audio keeps flowing, which is that rung's whole meaning.
 */

import { CallMediaKind } from '@migo/sdk';
import type { CallIce, CallSdp, Id, TurnServer } from '@migo/sdk';

import {
  decodeIceBatch,
  decodeSdpDescription,
  encodeIceBatch,
  encodeSdpDescription,
} from './call-signal.js';
import type { SdpDescription } from './call-signal.js';

// --- the quality ladder, mirrored from migo-sfu's decision core ------------------------------

/**
 * Where one link's video stands on the degradation ladder — `migo-sfu`'s `QualityStep`, in the
 * order section 165 pins: bitrate, then resolution, then frame rate, then video. Audio is never a
 * rung; the bottom of the ladder is "video off, audio alive".
 */
export type LinkQuality =
  'full' | 'bitrate-capped' | 'resolution-lowered' | 'frame-rate-lowered' | 'video-off';

/** The ladder, top rung first; the array's order *is* the ladder's order. */
export const QUALITY_LADDER: readonly LinkQuality[] = [
  'full',
  'bitrate-capped',
  'resolution-lowered',
  'frame-rate-lowered',
  'video-off',
];

/** The six numbers section 165 names, as one link's transport observes them. */
export interface LinkStats {
  /** Loss on the leg, whole percents. */
  packetLossPct: number;
  /** Round-trip time, milliseconds. */
  rttMs: number;
  /** Jitter, milliseconds. */
  jitterMs: number;
  /** Bandwidth the leg can still carry, kilobits per second. */
  availableKbps: number;
  /** What the leg is currently being sent, kilobits per second. */
  sentKbps: number;
  /** Frames the receiver dropped, whole percents. */
  droppedFramePct: number;
}

/** Where a link's congestion score crosses each rung; `migo-sfu`'s defaults, same numbers. */
export interface AdaptiveThresholds {
  rttBaselineMs: number;
  bitrateAt: number;
  resolutionAt: number;
  frameRateAt: number;
  videoOffAt: number;
}

/**
 * The default thresholds, mirroring `migo-sfu`'s `AdaptiveThresholds::default()`: RTT contributes
 * nothing below 120 ms, and the rungs cross at 12, 25, 40, and 60.
 */
export const DEFAULT_ADAPTIVE_THRESHOLDS: AdaptiveThresholds = {
  rttBaselineMs: 120,
  bitrateAt: 12,
  resolutionAt: 25,
  frameRateAt: 40,
  videoOffAt: 60,
};

/** Recovery climbs one rung per this many milliseconds; `migo-sfu`'s `RAMP_INTERVAL_MS`. */
export const RAMP_INTERVAL_MS = 3_000;

/** How many active video streams a call carries before the ninth is refused; the product limit. */
export const MAX_ACTIVE_VIDEO_STREAMS = 8;

/** The bitrate cap of the first rung down, as a percentage of what the link is sending. */
export const BITRATE_CAP_PCT = 60;
/** The bitrate cap once frame rate has been sacrificed, as a percentage of what is being sent. */
export const LOW_BITRATE_CAP_PCT = 40;

/**
 * Sums one link report into a single congestion score — `migo-sfu`'s `congestion_score`, the same
 * arithmetic, clamped at zero rather than saturating the way Rust's `u32` does (a report full of
 * impossible numbers still produces the worst score a rung can name, never a wrapped good one).
 */
export function congestionScore(stats: LinkStats, thresholds: AdaptiveThresholds): number {
  const loss = Math.max(0, Math.trunc(stats.packetLossPct)) * 4;
  const latency = Math.max(0, Math.trunc(stats.rttMs) - thresholds.rttBaselineMs) / 10;
  const jitter = Math.max(0, Math.trunc(stats.jitterMs)) / 5;
  const deficitKbps = Math.max(0, Math.trunc(stats.sentKbps) - Math.trunc(stats.availableKbps));
  const deficit = Math.trunc(deficitKbps / 100);
  const dropped = Math.max(0, Math.trunc(stats.droppedFramePct)) / 2;
  return Math.round(loss + latency + jitter + deficit + dropped);
}

/**
 * The rung a link report says the link belongs on — `migo-sfu`'s `target_step`. A worse score can
 * only name a lower rung, because the thresholds are in ladder order: the classification cannot
 * skip bitrate and jump to resolution.
 */
export function targetQuality(stats: LinkStats, thresholds: AdaptiveThresholds): LinkQuality {
  const score = congestionScore(stats, thresholds);
  if (score >= thresholds.videoOffAt) {
    return 'video-off';
  }
  if (score >= thresholds.frameRateAt) {
    return 'frame-rate-lowered';
  }
  if (score >= thresholds.resolutionAt) {
    return 'resolution-lowered';
  }
  if (score >= thresholds.bitrateAt) {
    return 'bitrate-capped';
  }
  return 'full';
}

/**
 * Moves a link from its current rung toward the target — `migo-sfu`'s `advance`: down the ladder is
 * immediate (the ladder's order is the order of what is given up, so landing on a low rung has
 * passed through the same sacrifices), up the ladder is one rung per `rampIntervalMs` since the
 * link last moved, never a jump — a jump is the oscillation the model exists to prevent.
 */
export function advanceQuality(
  current: LinkQuality,
  target: LinkQuality,
  changedAtMs: number,
  nowMs: number,
  rampIntervalMs: number,
): LinkQuality {
  const currentRung = QUALITY_LADDER.indexOf(current);
  const targetRung = QUALITY_LADDER.indexOf(target);
  if (targetRung >= currentRung) {
    return target;
  }
  if (nowMs - changedAtMs >= Math.max(1, rampIntervalMs)) {
    return QUALITY_LADDER[Math.max(0, currentRung - 1)] ?? current;
  }
  return current;
}

// --- the dialing rule ------------------------------------------------------------------------

/**
 * Whether this device dials the seat on `peerDevice`: the seat that joined *later* in the shared
 * join-order projection dials, and the earlier seat answers. Every client computes the same answer
 * from the same projection without a vote; a peer not in the projection is not ours to dial (their
 * own snapshot put us earlier, so they dial us).
 */
export function dialsRemote(
  seats: ReadonlyArray<{ userId: Id; deviceId: Id }>,
  me: { accountId: Id },
  peerDevice: Id,
): boolean {
  const peerIndex = seats.findIndex((seat) => seat.deviceId === peerDevice);
  if (peerIndex < 0) {
    return false;
  }
  const myIndex = seats.findIndex((seat) => seat.userId === me.accountId);
  if (myIndex < 0) {
    return false;
  }
  return myIndex > peerIndex;
}

/**
 * Who keeps their offer when both sides dialed at once (the projections of two concurrent joins can
 * disagree for a moment): the lexicographically smaller device id wins, so exactly one side of the
 * glare computes `true` and rolls back to answer.
 */
export function iKeepMyOffer(myDevice: Id, peerDevice: Id): boolean {
  return myDevice < peerDevice;
}

/**
 * Whether this device may publish video: it wants to, and fewer than the product limit's worth of
 * video streams would flow with its own added. In the mesh each seat enforces the cap from what
 * it can see, and what it can see at join time is the roster — the wire carries no media kind —
 * so the count is the remote seats, an upper bound on the video publishers among them. The bound
 * is therefore conservative where the SFU core's would be exact (a seat joining a roster of eight
 * refuses its video even if only three of the eight publish), and honest in the way that matters:
 * a call never carries more active video streams than the product limit, and a refused seat stays
 * in the call as audio — the stream is refused, never the participant.
 */
export function videoAdmitted(remoteSeats: number, wantsVideo: boolean): boolean {
  return wantsVideo && remoteSeats < MAX_ACTIVE_VIDEO_STREAMS;
}

// --- what the stats of a real peer connection become ------------------------------------------

/**
 * One raw reading of a peer connection's cumulative counters, as `getStats()` reports them.
 * Cumulative on purpose: loss and drop percentages are honest only as deltas between two readings,
 * never as lifetime totals over a call that started healthy.
 */
export interface RawLinkCounters {
  at: number;
  packetsLost: number;
  packetsReceived: number;
  framesReceived: number;
  framesDropped: number;
  bytesSent: number;
  rttMs: number;
  jitterMs: number;
  /** An outgoing-bandwidth estimate, when the browser reports one; `null` when it does not. */
  availableKbps: number | null;
  /**
   * Whether the nominated pair is a relay pair — the media is going through TURN rather than
   * straight between the two devices. Not a rung and not a score term: section 180 asks for it as a
   * *product* number, the share of calls that fell back, because that share is what says where the
   * next relay belongs. `undefined` when the connection reports no nominated pair yet, which is not
   * the same fact as `false`.
   */
  relay?: boolean;
}

/**
 * The stats of the stretch between two raw readings, or `null` when the stretch is too short or
 * too empty to say anything (the first reading of a link has no stretch before it).
 *
 * The mapping is honest about what a browser measures: loss, RTT, jitter, sent bitrate, and dropped
 * frames are real; a bandwidth *estimate* is only read where the browser reports one
 * (`availableOutgoingBitrate` on the nominated candidate pair — a de-facto standard surface, not a
 * promised one), and where it does not, available equals sent so the deficit term contributes
 * nothing rather than inventing a number. Four measured signals still drive the ladder; the model
 * was built to classify client-reported numbers, and these are numbers a client can truly report.
 */
export function linkStatsBetween(prev: RawLinkCounters, next: RawLinkCounters): LinkStats | null {
  const seconds = (next.at - prev.at) / 1000;
  if (seconds <= 0) {
    return null;
  }
  const lost = next.packetsLost - prev.packetsLost;
  const received = next.packetsReceived - prev.packetsReceived;
  const lossDenominator = lost + received;
  if (lossDenominator <= 0) {
    return null;
  }
  const frames = next.framesReceived - prev.framesReceived;
  const dropped = next.framesDropped - prev.framesDropped;
  const sentKbps = Math.max(0, ((next.bytesSent - prev.bytesSent) * 8) / 1000 / seconds);
  return {
    packetLossPct: Math.round((Math.max(0, lost) / lossDenominator) * 100),
    rttMs: Math.max(0, Math.round(next.rttMs)),
    jitterMs: Math.max(0, Math.round(next.jitterMs)),
    availableKbps: next.availableKbps ?? sentKbps,
    sentKbps: Math.round(sentKbps),
    droppedFramePct:
      frames + dropped > 0 ? Math.round((Math.max(0, dropped) / (frames + dropped)) * 100) : 0,
  };
}

/**
 * Reads one raw sample of a real peer connection's counters, or `null` when the connection reports
 * nothing usable yet. Kept separate from {@link linkStatsBetween} because reading is the browser's
 * half and the delta is the model's — only the model's half is pure enough to pin without a
 * connection. The relay fact rides along because it is free here and impossible anywhere else: only
 * this reading sees the nominated pair and the two candidates it names.
 */
export async function readLinkCounters(pc: RTCPeerConnection): Promise<RawLinkCounters | null> {
  const stats = await pc.getStats();
  let rttMs = 0;
  let jitterMs = 0;
  let availableKbps: number | null = null;
  let relay: boolean | undefined;
  const totals = {
    packetsLost: 0,
    packetsReceived: 0,
    framesReceived: 0,
    framesDropped: 0,
    bytesSent: 0,
  };
  // Candidates are named by id from the nominated pair, so they have to be read before it is:
  // `getStats()` yields in no promised order, and a single pass that met the pair first would find
  // no candidate to look up. Collected first, interpreted after.
  const candidateTypes = new Map<string, string>();
  for (const report of stats.values()) {
    const typed = report as Partial<RTCIceCandidateStats> & { id?: string; type?: string };
    if (typed.type === 'local-candidate' || typed.type === 'remote-candidate') {
      if (typeof typed.id === 'string') {
        candidateTypes.set(typed.id, typed.candidateType ?? '');
      }
    }
  }
  let sawAnything = false;
  for (const report of stats.values()) {
    const typed = report as Partial<RTCInboundRtpStreamStats> &
      Partial<RTCIceCandidatePairStats> &
      Partial<RTCOutboundRtpStreamStats>;
    if (typed.type === 'inbound-rtp') {
      sawAnything = true;
      totals.packetsLost += Math.max(0, typed.packetsLost ?? 0);
      totals.packetsReceived += typed.packetsReceived ?? 0;
      totals.framesReceived += typed.framesReceived ?? 0;
      totals.framesDropped += typed.framesDropped ?? 0;
      jitterMs = Math.max(jitterMs, Math.round((typed.jitter ?? 0) * 1000));
    } else if (typed.type === 'outbound-rtp') {
      sawAnything = true;
      totals.bytesSent += typed.bytesSent ?? 0;
    } else if (typed.type === 'candidate-pair' && typed.state === 'succeeded' && typed.nominated) {
      sawAnything = true;
      rttMs = Math.round((typed.currentRoundTripTime ?? 0) * 1000);
      const estimate = (typed as { availableOutgoingBitrate?: number }).availableOutgoingBitrate;
      if (typeof estimate === 'number' && estimate > 0) {
        availableKbps = Math.round(estimate / 1000);
      }
      // Either end being a relay candidate means the media rides TURN: a relay pair is one whose
      // local *or* remote candidate was reflexive-from-a-relay, and both ends read it the same way.
      const local = candidateTypes.get(typed.localCandidateId ?? '');
      const remote = candidateTypes.get(typed.remoteCandidateId ?? '');
      if (local !== undefined || remote !== undefined) {
        relay = local === 'relay' || remote === 'relay';
      }
    }
  }
  if (!sawAnything) {
    return null;
  }
  return { at: Date.now(), ...totals, rttMs, jitterMs, availableKbps, relay };
}

/**
 * The sender shaping one rung of the ladder asks of this endpoint's own video on one link.
 *
 * `replaceTrack(null)` (the `enabled: false` rung) is per-link by construction — the same local
 * camera keeps flowing to every link the ladder has not grounded — while bitrate, resolution, and
 * frame-rate caps ride the sender's parameters. The frame-rate rung halves the frame rate
 * (`maxFramerate` 15 for a 30 fps sender) where the SFU core drops every second frame by sequence:
 * a sender cannot stride its own frames, but half the rate is the same sacrifice on the wire.
 */
export function videoSenderParams(
  quality: LinkQuality,
  measuredKbps: number,
): {
  enabled: boolean;
  maxBitrate?: number;
  scaleResolutionDownBy?: number;
  maxFramerate?: number;
} {
  switch (quality) {
    case 'full':
      return { enabled: true };
    case 'bitrate-capped':
      return {
        enabled: true,
        maxBitrate: Math.max(60, Math.round((measuredKbps * BITRATE_CAP_PCT) / 100)),
      };
    case 'resolution-lowered':
      return {
        enabled: true,
        maxBitrate: Math.max(60, Math.round((measuredKbps * BITRATE_CAP_PCT) / 100)),
        scaleResolutionDownBy: 2,
      };
    case 'frame-rate-lowered':
      return {
        enabled: true,
        maxBitrate: Math.max(60, Math.round((measuredKbps * LOW_BITRATE_CAP_PCT) / 100)),
        scaleResolutionDownBy: 2,
        maxFramerate: 15,
      };
    case 'video-off':
      return { enabled: false };
    default: {
      const unreachable: never = quality;
      return unreachable;
    }
  }
}

/**
 * Shapes one video sender to a rung, and answers whether the sender now carries the track.
 *
 * The one place a rung becomes something a peer connection does, shared by both planes that run the
 * ladder — the group plane's per-link senders and the 1:1 plane's single sender — so a rung cannot
 * mean one thing in a group call and another in a two-party one. The caller owns the `attached`
 * flag because only it knows whether it ever attached a track; this function keeps it honest.
 *
 * `track` is the camera the caller *wants* attached, which is not always the camera it has: a
 * caller whose camera is off passes `null` and the sender is left without a track whatever the rung
 * says, because the rung decides how much video to send and the user decides whether to send any.
 * Turning a track off is `replaceTrack(null)` rather than `track.enabled = false` for the reason
 * the group plane gives: the same camera keeps flowing to every link the ladder has not grounded,
 * and the ones it has grounded pay nothing for a stream they are not being sent.
 */
export function shapeVideoSender(
  sender: RTCRtpSender,
  track: MediaStreamTrack | null,
  quality: LinkQuality,
  measuredKbps: number,
  attached: boolean,
): boolean {
  const params = videoSenderParams(quality, measuredKbps);
  if (!params.enabled || track === null) {
    if (attached) {
      void sender.replaceTrack(null).catch(() => {});
    }
    return false;
  }
  let carrying = attached;
  if (!carrying) {
    void sender.replaceTrack(track).catch(() => {});
    carrying = true;
  }
  const current = sender.getParameters();
  const encoding = current.encodings[0] ?? {};
  if (params.maxBitrate !== undefined) {
    encoding.maxBitrate = params.maxBitrate * 1000;
  }
  if (params.scaleResolutionDownBy !== undefined) {
    encoding.scaleResolutionDownBy = params.scaleResolutionDownBy;
  }
  if (params.maxFramerate !== undefined) {
    encoding.maxFramerate = params.maxFramerate;
  }
  current.encodings = [encoding];
  void sender.setParameters(current).catch(() => {
    // A sender that cannot be shaped keeps sending; the next tick retries the rung it is on.
  });
  return carrying;
}

// --- the ICE servers of a group call ----------------------------------------------------------

/** The public STUN fallback every peer connection carries, as the 1:1 plane's does. */
export const GROUP_STUN_FALLBACK: RTCIceServer = { urls: 'stun:stun.l.google.com:19302' };

/**
 * The ICE servers for a group call's peer connections: the TURN relays the join reply carried
 * (short-lived credentials, never embedded in the client), then the public STUN fallback. An empty
 * relay list still yields the fallback — a link that only needed STUN must not be refused because
 * the relay list was empty.
 */
export function groupIceServers(servers: readonly TurnServer[]): RTCIceServer[] {
  const iceServers: RTCIceServer[] = servers.map((server) => ({
    urls: server.url,
    ...(server.username !== '' ? { username: server.username } : {}),
    ...(server.credential !== '' ? { credential: server.credential } : {}),
  }));
  iceServers.push(GROUP_STUN_FALLBACK);
  return iceServers;
}

// --- the plane itself ------------------------------------------------------------------------

/** One link's phase, as the roster screen states it. */
export type GroupMediaLinkPhase = 'connecting' | 'connected' | 'failed' | 'closed';

/** One remote seat's link, as the UI renders it: the phase, the flowing media, the rung. */
export interface GroupMediaLink {
  userId: Id;
  deviceId: Id;
  phase: GroupMediaLinkPhase;
  /** A remote audio track is flowing. */
  audio: boolean;
  /** A remote video track is flowing. */
  video: boolean;
  /** This endpoint's send quality on the link, top rung while unmeasured. */
  quality: LinkQuality;
}

/** Everything the plane needs from its host; every browser-shaped edge is an injection point. */
export interface GroupMediaPlaneDeps {
  callId: Id;
  conversationId: Id;
  accountId: Id;
  deviceId: Id;
  /** What the join carried; a video call publishes a camera within the product limit. */
  mediaKind: CallMediaKind;
  /** The ICE servers of the call's links; the join reply's TURN list, plus the STUN fallback. */
  iceServers: RTCIceServer[];
  /** Builds one peer connection per remote device. */
  createPeer: (iceServers: RTCIceServer[]) => RTCPeerConnection;
  /** Acquires the local microphone (and camera, when video is published). */
  acquire: (kind: CallMediaKind) => Promise<MediaStream>;
  /** Sends one sealed SDP description to a roster device (`CALL_SDP`). */
  sendSdp: (toDevice: Id, sealed: Uint8Array) => Promise<void>;
  /** Sends one sealed ICE batch to a roster device (`CALL_ICE`). */
  sendIce: (toDevice: Id, sealed: Uint8Array) => Promise<void>;
  /** Seals one signaling frame under the call's frame key; the SDK's `callKeys.sealFrame`. */
  seal: (frame: Uint8Array) => Uint8Array;
  /** Opens one sealed signaling frame; the SDK's `callKeys.openFrame`. */
  open: (sealed: Uint8Array) => Uint8Array;
  /** Called whenever the link projection changes, with the full new projection. */
  onLinks?: (links: GroupMediaLink[]) => void;
  /** Where failures are stated, as facts. */
  onFailure?: (what: string) => void;
  /** How long gathered ICE candidates linger before one relay carries them. */
  iceLingerMs?: number;
  /** The thresholds of the quality ladder; the SFU core's defaults when absent. */
  thresholds?: AdaptiveThresholds;
  /** How recovery climbs; `RAMP_INTERVAL_MS` when absent. */
  rampIntervalMs?: number;
  /** Reads one link's raw counters; the real `getStats()` reader when absent. */
  readCounters?: (pc: RTCPeerConnection) => Promise<RawLinkCounters | null>;
}

/** What failed before any media existed, said as a fact. */
export const GROUP_MEDIA_MIC_FAILED = 'Microphone unavailable. Check permissions and try again.';

/** How long the ICE batch of one link lingers before one relay carries it (section 165: batch). */
const DEFAULT_ICE_LINGER_MS = 250;

/** One remote device's link: the peer connection, the negotiation bookkeeping, and the rung. */
interface PeerLink {
  userId: Id;
  deviceId: Id;
  pc: RTCPeerConnection;
  /** Whether this device created the link by dialing (as opposed to answering an offer). */
  iDialed: boolean;
  phase: GroupMediaLinkPhase;
  quality: LinkQuality;
  qualityChangedAt: number;
  /** The last measured send bitrate on the link, in kbps — the base the caps are a percentage of. */
  sentKbps: number;
  remoteDescriptionSet: boolean;
  /** This device's video sender on the link, kept because `replaceTrack(null)` empties it. */
  videoSender: RTCRtpSender | null;
  /** Whether the video sender currently carries the local camera track. */
  sendingVideo: boolean;
  /** Candidates gathered but not yet relayed, waiting for the linger or gathering's end. */
  iceBatch: RTCIceCandidateInit[];
  iceTimer: ReturnType<typeof setTimeout> | null;
  /** Candidates the peer relayed before this side's remote description was set. */
  heldIce: RTCIceCandidateInit[];
  /** The remote stream once the peer's tracks arrive. */
  remoteStream: MediaStream | null;
  /** The previous raw counters reading, for the delta the ladder classifies. */
  lastCounters: RawLinkCounters | null;
}

/**
 * The media plane of one seated group call. Created when the seat is accepted *and* the call's
 * frame key is held, fed the roster projection and the sealed relays by its host, and torn down by
 * {@link leave} on every exit path. All browser surfaces arrive through {@link
 * GroupMediaPlaneDeps}, so the whole negotiation runs in a test over fake peers and a fake relay
 * without a socket, a microphone, or a DOM.
 */
export class GroupMediaPlane {
  readonly #deps: GroupMediaPlaneDeps;
  readonly #iceLingerMs: number;
  readonly #thresholds: AdaptiveThresholds;
  readonly #rampIntervalMs: number;
  readonly #readCounters: (pc: RTCPeerConnection) => Promise<RawLinkCounters | null>;

  #seats: ReadonlyArray<{ userId: Id; deviceId: Id }> = [];
  #links = new Map<Id, PeerLink>();
  #localStream: MediaStream | null = null;
  #localVideoTrack: MediaStreamTrack | null = null;
  #videoPublished = false;
  #muted = false;
  #cameraOn = true;
  #stopped = false;

  constructor(deps: GroupMediaPlaneDeps) {
    this.#deps = deps;
    this.#iceLingerMs = deps.iceLingerMs ?? DEFAULT_ICE_LINGER_MS;
    this.#thresholds = deps.thresholds ?? DEFAULT_ADAPTIVE_THRESHOLDS;
    this.#rampIntervalMs = deps.rampIntervalMs ?? RAMP_INTERVAL_MS;
    this.#readCounters = deps.readCounters ?? readLinkCounters;
  }

  /**
   * Acquires the local media and dials every seat this device dials.
   *
   * A microphone that cannot be acquired is a failure the host states as a fact — a group call
   * without a mic is a spectator seat this build does not pretend to have. A camera that cannot be
   * acquired (or is refused by the product limit) degrades the seat to audio, the same honesty the
   * 1:1 answer path keeps: the conversation continues, the screen says what it has.
   */
  async begin(seats: ReadonlyArray<{ userId: Id; deviceId: Id }>): Promise<void> {
    if (this.#stopped || this.#localStream !== null) {
      return;
    }
    this.#seats = seats;
    const wantsVideo = this.#deps.mediaKind === CallMediaKind.Video;
    let stream: MediaStream;
    try {
      stream = await this.#deps.acquire(this.#deps.mediaKind);
    } catch {
      this.#deps.onFailure?.(GROUP_MEDIA_MIC_FAILED);
      return;
    }
    this.#localStream = stream;
    this.#localVideoTrack = stream.getVideoTracks()[0] ?? null;
    // The product limit, enforced from this seat's view of the roster — an upper bound on the
    // video publishers among the remote seats, since the wire carries no media kind. A seat the
    // bound refuses stays in the call as audio: the stream is refused, never the seat.
    const remoteSeats = seats.filter((seat) => seat.userId !== this.#deps.accountId).length;
    this.#videoPublished = videoAdmitted(remoteSeats, this.#localVideoTrack !== null && wantsVideo);
    if (this.#videoPublished && this.#localVideoTrack !== null) {
      this.#cameraOn = this.#localVideoTrack.enabled;
    } else {
      this.#localVideoTrack?.stop();
      // The refused track leaves the stream as well: a stopped track still lingers in the
      // stream's track list, and the roster screen reads that list — what it must never see
      // is a camera track this seat was refused. The stream stays, honest: audio-only.
      if (this.#localVideoTrack !== null) {
        this.#localStream.removeTrack(this.#localVideoTrack);
      }
      this.#localVideoTrack = null;
    }
    for (const seat of seats) {
      if (seat.userId === this.#deps.accountId || seat.deviceId === this.#deps.deviceId) {
        continue;
      }
      if (dialsRemote(seats, { accountId: this.#deps.accountId }, seat.deviceId)) {
        await this.#dial(seat);
      }
    }
    this.#emit();
  }

  /**
   * The roster moved. Links to departed accounts close (a peer's seat replacement is a departure
   * followed by an arrival, and the replacing device dials fresh); a seat this device dials that is
   * new to the projection does not exist yet in practice — new arrivals are appended at the end of
   * the join order, so they dial us, not the other way.
   */
  seatsChanged(seats: ReadonlyArray<{ userId: Id; deviceId: Id }>): void {
    if (this.#stopped) {
      return;
    }
    this.#seats = seats;
    const seatedDevices = new Set(
      seats.filter((seat) => seat.userId !== this.#deps.accountId).map((seat) => seat.deviceId),
    );
    for (const [deviceId, link] of this.#links) {
      if (!seatedDevices.has(deviceId)) {
        this.#closeLink(link);
        this.#links.delete(deviceId);
      }
    }
    // A peer that replaced another account's seat while we held a link to the old device: the
    // departure above closed it. Nothing dials here — see the method doc.
    this.#emit();
  }

  /**
   * The call's frame key changed state. A first-held key is what made sealing possible (the host
   * does not start the plane without one); an epoch advance invalidates the sealing of every
   * negotiation still in flight — the epoch is the AEAD's associated data — so links that have not
   * connected are closed and re-dialed by whichever side dials, under the fresh key. Connected
   * links keep flowing: their media rides DTLS-SRTP, which the frame key does not gate.
   */
  keyEpochChanged(): void {
    if (this.#stopped) {
      return;
    }
    for (const [deviceId, link] of [...this.#links]) {
      if (link.phase === 'connecting') {
        const seat = this.#seats.find((entry) => entry.deviceId === deviceId);
        this.#closeLink(link);
        this.#links.delete(deviceId);
        if (
          seat !== undefined &&
          dialsRemote(this.#seats, { accountId: this.#deps.accountId }, deviceId)
        ) {
          void this.#dial(seat);
        }
      }
    }
    this.#emit();
  }

  /** One sealed SDP relay for this call: an offer to answer, or an answer to an offer of ours. */
  async onSdp(event: CallSdp): Promise<void> {
    if (
      this.#stopped ||
      event.callId !== this.#deps.callId ||
      event.toDevice !== this.#deps.deviceId
    ) {
      return;
    }
    let description: SdpDescription;
    try {
      description = decodeSdpDescription(this.#deps.open(event.sealedSdp));
    } catch {
      // Not ours to open: a key ask or answer rides this opcode sealed the session way, and a frame
      // this key cannot open belongs to a stranded negotiation the key change will reset. Either
      // way the signaling layer's frames are left for it, silently.
      return;
    }
    let link = this.#links.get(event.fromDevice);
    if (link === undefined) {
      const seat = this.#seats.find((entry) => entry.deviceId === event.fromDevice);
      if (seat === undefined || description.type !== 'offer') {
        // An offer from a device not in the projection is a frame this seat has no business with.
        return;
      }
      if (dialsRemote(this.#seats, { accountId: this.#deps.accountId }, event.fromDevice)) {
        // Both sides dialed — concurrent joins whose projections disagree. The glare breaks by
        // device id: the loser rolls back to answer, the winner ignores this offer and waits for
        // the answer its own offer is owed.
        if (iKeepMyOffer(this.#deps.deviceId, event.fromDevice)) {
          return;
        }
      }
      link = this.#answer(seat);
    }
    if (description.type === 'offer') {
      if (link.remoteDescriptionSet) {
        // A renegotiated offer is a future flow; this build holds one description per link.
        return;
      }
      try {
        await link.pc.setRemoteDescription(description);
        link.remoteDescriptionSet = true;
        this.#drainHeldIce(link);
        const answer = await link.pc.createAnswer();
        await link.pc.setLocalDescription(answer);
        await this.#deps.sendSdp(
          event.fromDevice,
          this.#deps.seal(encodeSdpDescription({ type: 'answer', sdp: answer.sdp ?? '' })),
        );
      } catch {
        // The negotiation failed on this end; the link stays connecting until the peer gives up or
        // a key change resets it. A fact on the screen beats a silent half-open promise.
        return;
      }
      this.#flushIce(link);
      return;
    }
    if (description.type === 'answer' && link.iDialed && !link.remoteDescriptionSet) {
      try {
        await link.pc.setRemoteDescription(description);
        link.remoteDescriptionSet = true;
        this.#drainHeldIce(link);
      } catch {
        return;
      }
      this.#flushIce(link);
    }
  }

  /** One sealed ICE batch for this call: applied now, or held until the remote description exists. */
  onIce(event: CallIce): void {
    if (
      this.#stopped ||
      event.callId !== this.#deps.callId ||
      event.toDevice !== this.#deps.deviceId
    ) {
      return;
    }
    const link = this.#links.get(event.fromDevice);
    if (link === undefined) {
      return;
    }
    let candidates: RTCIceCandidateInit[];
    try {
      candidates = decodeIceBatch(this.#deps.open(event.sealedCandidates));
    } catch {
      return;
    }
    if (link.remoteDescriptionSet) {
      for (const candidate of candidates) {
        link.pc.addIceCandidate(candidate).catch(() => {
          // A candidate the connection no longer wants is normal near the end of gathering.
        });
      }
    } else {
      link.heldIce.push(...candidates);
    }
  }

  /**
   * One turn of the quality ladder: every connected link's counters are read, the delta classified,
   * and the rung moved — down to the target at once, up one rung per interval — with the sender
   * shaped to wherever it landed. The host drives this on a timer; tests drive it by hand.
   */
  async tick(nowMs: number): Promise<void> {
    if (this.#stopped) {
      return;
    }
    for (const link of this.#links.values()) {
      if (link.phase !== 'connected') {
        continue;
      }
      const counters = await this.#readCounters(link.pc).catch(() => null);
      if (counters === null) {
        continue;
      }
      const previous = link.lastCounters;
      link.lastCounters = counters;
      if (previous === null) {
        continue;
      }
      const stats = linkStatsBetween(previous, counters);
      if (stats === null) {
        continue;
      }
      link.sentKbps = stats.sentKbps;
      const target = targetQuality(stats, this.#thresholds);
      const next = advanceQuality(
        link.quality,
        target,
        link.qualityChangedAt,
        nowMs,
        this.#rampIntervalMs,
      );
      if (next !== link.quality) {
        link.quality = next;
        link.qualityChangedAt = nowMs;
        this.#shapeVideo(link);
      }
    }
    this.#emit();
  }

  /** Mutes or unmutes this seat's microphone, everywhere at once — a user's mic is one fact. */
  toggleMute(): boolean {
    this.#muted = !this.#muted;
    for (const track of this.#localStream?.getAudioTracks() ?? []) {
      track.enabled = !this.#muted;
    }
    return this.#muted;
  }

  /** Whether this seat's microphone is muted. */
  get muted(): boolean {
    return this.#muted;
  }

  /**
   * Turns this seat's camera on or off, everywhere at once. A camera that was never published
   * (refused by the product limit, or an audio call) has no toggle to give — `null` says that, so
   * a screen never shows a button that does nothing.
   */
  toggleCamera(): boolean | null {
    if (!this.#videoPublished || this.#localVideoTrack === null) {
      return null;
    }
    this.#cameraOn = !this.#cameraOn;
    this.#localVideoTrack.enabled = this.#cameraOn;
    return this.#cameraOn;
  }

  /** Whether this seat's camera is on; `null` when no camera was published. */
  get cameraOn(): boolean | null {
    if (!this.#videoPublished || this.#localVideoTrack === null) {
      return null;
    }
    return this.#cameraOn;
  }

  /** This seat's local stream, for the self-view; `null` before media exists. */
  get localStream(): MediaStream | null {
    return this.#localStream;
  }

  /** Whether this seat publishes video (it wanted to, and the product limit admitted it). */
  get videoPublished(): boolean {
    return this.#videoPublished;
  }

  /** The remote stream of one device, once its tracks arrive. */
  remoteStreamOf(deviceId: Id): MediaStream | null {
    return this.#links.get(deviceId)?.remoteStream ?? null;
  }

  /** The current link projection, in join order. */
  links(): GroupMediaLink[] {
    return [...this.#links.values()]
      .sort((a, b) => {
        const aIndex = this.#seats.findIndex((seat) => seat.deviceId === a.deviceId);
        const bIndex = this.#seats.findIndex((seat) => seat.deviceId === b.deviceId);
        return (
          (aIndex < 0 ? this.#seats.length : aIndex) - (bIndex < 0 ? this.#seats.length : bIndex)
        );
      })
      .map((link) => ({
        userId: link.userId,
        deviceId: link.deviceId,
        phase: link.phase,
        audio: (link.remoteStream?.getAudioTracks().length ?? 0) > 0,
        video: (link.remoteStream?.getVideoTracks().length ?? 0) > 0,
        quality: link.quality,
      }));
  }

  /** Tears the plane down: links closed, local tracks stopped. Idempotent. */
  leave(): void {
    if (this.#stopped) {
      return;
    }
    this.#stopped = true;
    for (const link of this.#links.values()) {
      this.#closeLink(link);
    }
    // One last projection, said as a fact before the plane goes quiet: every link this seat held is
    // closed. The stopped guard silences every later emit and this is deliberately the exception —
    // a host that renders from onLinks would otherwise be left showing seats that are gone, because
    // the links are about to leave the projection entirely.
    this.#deps.onLinks?.(this.links());
    this.#links.clear();
    for (const track of this.#localStream?.getTracks() ?? []) {
      track.stop();
    }
    this.#localStream = null;
    this.#localVideoTrack = null;
  }

  // --- the internals -------------------------------------------------------------------------

  /** Dials one seat: a fresh peer connection, the local tracks, a sealed offer. */
  async #dial(seat: { userId: Id; deviceId: Id }): Promise<void> {
    if (this.#stopped || this.#links.has(seat.deviceId)) {
      return;
    }
    const link = this.#createLink(seat, true);
    this.#links.set(seat.deviceId, link);
    try {
      const offer = await link.pc.createOffer();
      await link.pc.setLocalDescription(offer);
      await this.#deps.sendSdp(
        seat.deviceId,
        this.#deps.seal(encodeSdpDescription({ type: 'offer', sdp: offer.sdp ?? '' })),
      );
    } catch {
      // The offer never left; closing the half-built link lets a later roster movement retry.
      this.#closeLink(link);
      this.#links.delete(seat.deviceId);
    }
  }

  /** Creates the answering half of a link an offer just opened. */
  #answer(seat: { userId: Id; deviceId: Id }): PeerLink {
    const link = this.#createLink(seat, false);
    this.#links.set(seat.deviceId, link);
    return link;
  }

  /** Builds one link's peer connection and wires its handlers to the plane's bookkeeping. */
  #createLink(seat: { userId: Id; deviceId: Id }, iDialed: boolean): PeerLink {
    const pc = this.#deps.createPeer(this.#deps.iceServers);
    const link: PeerLink = {
      userId: seat.userId,
      deviceId: seat.deviceId,
      pc,
      iDialed,
      phase: 'connecting',
      quality: 'full',
      qualityChangedAt: 0,
      sentKbps: 0,
      remoteDescriptionSet: false,
      videoSender: null,
      sendingVideo: false,
      iceBatch: [],
      iceTimer: null,
      heldIce: [],
      remoteStream: null,
      lastCounters: null,
    };
    pc.onicecandidate = (event: RTCPeerConnectionIceEvent): void => {
      if (event.candidate === null) {
        // Gathering finished: whatever is batched is all there will be.
        this.#flushIce(link);
        return;
      }
      link.iceBatch.push(event.candidate.toJSON());
      if (link.iceTimer === null) {
        link.iceTimer = setTimeout(() => {
          link.iceTimer = null;
          this.#flushIce(link);
        }, this.#iceLingerMs);
      }
    };
    pc.ontrack = (event: RTCTrackEvent): void => {
      const stream = event.streams[0];
      if (stream !== undefined) {
        link.remoteStream = stream;
        this.#emit();
      }
    };
    pc.onconnectionstatechange = (): void => {
      if (pc.connectionState === 'connected') {
        link.phase = 'connected';
      } else if (pc.connectionState === 'failed') {
        link.phase = 'failed';
      } else if (pc.connectionState === 'closed') {
        link.phase = 'closed';
      } else if (link.phase !== 'failed' && link.phase !== 'closed') {
        link.phase = 'connecting';
      }
      this.#emit();
    };
    for (const track of this.#localStream?.getTracks() ?? []) {
      const sender = pc.addTrack(track, this.#localStream as MediaStream);
      if (track.kind === 'video') {
        link.videoSender = sender;
        link.sendingVideo = true;
      }
    }
    return link;
  }

  /** Sends the link's batched candidates, if there are any. */
  #flushIce(link: PeerLink): void {
    if (this.#stopped || link.iceBatch.length === 0) {
      return;
    }
    const batch = link.iceBatch;
    link.iceBatch = [];
    void this.#deps.sendIce(link.deviceId, this.#deps.seal(encodeIceBatch(batch))).catch(() => {
      // A lost batch is recovered by the next one; never fatal.
    });
  }

  /** Applies the candidates the peer sent before this side's remote description existed. */
  #drainHeldIce(link: PeerLink): void {
    if (!link.remoteDescriptionSet) {
      return;
    }
    const held = link.heldIce;
    link.heldIce = [];
    for (const candidate of held) {
      link.pc.addIceCandidate(candidate).catch(() => {
        // A candidate the connection no longer wants is normal near the end of gathering.
      });
    }
  }

  /**
   * Shapes this endpoint's video sender on one link to the link's rung. The video-off rung removes
   * the track from *this* sender only — the same camera keeps flowing to every link the ladder has
   * not grounded — and the caps ride the sender's parameters. The work itself is
   * {@link shapeVideoSender}, which the 1:1 plane runs too, so a rung cannot come to mean one thing
   * here and another there.
   */
  #shapeVideo(link: PeerLink): void {
    const sender = link.videoSender;
    if (sender === null || this.#localVideoTrack === null) {
      return;
    }
    link.sendingVideo = shapeVideoSender(
      sender,
      this.#cameraOn ? this.#localVideoTrack : null,
      link.quality,
      link.sentKbps,
      link.sendingVideo,
    );
  }

  /** Closes one link's connection and disarms its timer. */
  #closeLink(link: PeerLink): void {
    if (link.iceTimer !== null) {
      clearTimeout(link.iceTimer);
      link.iceTimer = null;
    }
    link.pc.onicecandidate = null;
    link.pc.ontrack = null;
    link.pc.onconnectionstatechange = null;
    link.pc.close();
    link.phase = 'closed';
  }

  /** Announces the current projection to the host. */
  #emit(): void {
    if (!this.#stopped) {
      this.#deps.onLinks?.(this.links());
    }
  }
}
