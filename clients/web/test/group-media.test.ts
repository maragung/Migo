/**
 * What the group call's media plane is allowed to do, and what it must never fake.
 *
 * The tests pin four layers:
 *
 *   1. **The quality ladder, number for number.** The ladder is `migo-sfu`'s decision core
 *      mirrored client-side, so every constant and every threshold is pinned to the crate's own
 *      arithmetic: the congestion score's weights, the rung a score lands on, degradation landing
 *      on its target at once while recovery climbs one rung per ramp interval, and the sender
 *      shaping each rung asks for.
 *   2. **The dialing rule.** The seat that joined later dials, the earlier seat answers, glare
 *      breaks by device id, and a roster bigger than the video limit refuses this seat's *stream*,
 *      never its seat.
 *   3. **Two participants, end to end, over real frame keys.** Two planes, each sealing and
 *      opening through a real `CallKeyState` — the same AEAD, the same call-and-epoch binding the
 *      SDK's key domain uses — with SDP and ICE relayed through a virtual wire between fake peer
 *      connections. The fake transport carries no media; what it proves is the negotiation: who
 *      dials, what is sealed, when the remote stream exists, and that the tracks and the toggles
 *      touch only what they should.
 *   4. **The roster moving under a live call.** A third seat dials both, a rotation strands the
 *      negotiations still in flight and the dialer re-dials under the fresh key, a departure
 *      closes exactly its own links, and the quality ladder degrades and recovers a live video
 *      sender through the real delta math.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { CallKeyState, CallMediaKind, newId } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import {
  DEFAULT_ADAPTIVE_THRESHOLDS,
  GroupMediaPlane,
  MAX_ACTIVE_VIDEO_STREAMS,
  QUALITY_LADDER,
  RAMP_INTERVAL_MS,
  advanceQuality,
  congestionScore,
  dialsRemote,
  groupIceServers,
  iKeepMyOffer,
  linkStatsBetween,
  readLinkCounters,
  targetQuality,
  videoAdmitted,
  videoSenderParams,
  videoTrackToSend,
} from '../src/lib/migo/group-media.js';
import type {
  AdaptiveThresholds,
  GroupMediaLink,
  GroupMediaPlaneDeps,
  LinkStats,
  RawLinkCounters,
} from '../src/lib/migo/group-media.js';

const T: AdaptiveThresholds = DEFAULT_ADAPTIVE_THRESHOLDS;

// The call key domain binds real 26-character ids, so the call id is a minted one — the short
// fixture labels are for the seats, which never enter the key derivations.
const CALL = newId();
const CONVERSATION = 'grp-conv' as Id;

const ACC_A = 'acc-a' as Id;
const ACC_B = 'acc-b' as Id;
const ACC_C = 'acc-c' as Id;
const DEV_A = 'dev-a' as Id;
const DEV_B = 'dev-b' as Id;
const DEV_C = 'dev-c' as Id;

const SEAT_A = { userId: ACC_A, deviceId: DEV_A };
const SEAT_B = { userId: ACC_B, deviceId: DEV_B };
const SEAT_C = { userId: ACC_C, deviceId: DEV_C };

// --- the ladder, number for number with migo-sfu ---------------------------------------------

test('the congestion score weighs the six numbers the way the SFU core does', () => {
  // loss*4 + (rtt-baseline)/10 + jitter/5 + deficitKbps/100 + dropped/2, each truncated where the
  // crate truncates, never wrapped: 20 + 8 + 6 + 5 + 5 = 44.
  const stats: LinkStats = {
    packetLossPct: 5,
    rttMs: 200,
    jitterMs: 30,
    availableKbps: 1000,
    sentKbps: 1500,
    droppedFramePct: 10,
  };
  assert.equal(congestionScore(stats, T), 44);
  // A quiet link scores zero, and an impossible report never wraps into a good score.
  assert.equal(
    congestionScore(
      {
        packetLossPct: 0,
        rttMs: 90,
        jitterMs: 0,
        availableKbps: 2000,
        sentKbps: 500,
        droppedFramePct: 0,
      },
      T,
    ),
    0,
  );
  assert.ok(congestionScore({ ...stats, packetLossPct: 90, rttMs: 900, jitterMs: 400 }, T) > 60);
});

test('a score lands on its rung at the crate’s own thresholds', () => {
  const quiet: LinkStats = {
    packetLossPct: 0,
    rttMs: 90,
    jitterMs: 0,
    availableKbps: 2000,
    sentKbps: 500,
    droppedFramePct: 0,
  };
  const at = (score: number): LinkStats => ({
    ...quiet,
    // Latency alone drives the score, one point per 10 ms past the baseline — whole points, so
    // the rung named is the threshold's own rather than a fraction the truncation rounds away.
    rttMs: T.rttBaselineMs + score * 10,
  });
  assert.equal(targetQuality(at(0), T), 'full');
  assert.equal(targetQuality(at(11), T), 'full');
  assert.equal(targetQuality(at(12), T), 'bitrate-capped');
  assert.equal(targetQuality(at(24), T), 'bitrate-capped');
  assert.equal(targetQuality(at(25), T), 'resolution-lowered');
  assert.equal(targetQuality(at(39), T), 'resolution-lowered');
  assert.equal(targetQuality(at(40), T), 'frame-rate-lowered');
  assert.equal(targetQuality(at(59), T), 'frame-rate-lowered');
  assert.equal(targetQuality(at(60), T), 'video-off');
  // The ladder's order is the array's order, top rung first.
  assert.deepEqual(QUALITY_LADDER, [
    'full',
    'bitrate-capped',
    'resolution-lowered',
    'frame-rate-lowered',
    'video-off',
  ]);
  assert.equal(RAMP_INTERVAL_MS, 3_000, 'the ramp interval is the crate’s own');
});

test('degradation lands on its target at once; recovery climbs one rung per interval', () => {
  // Down is immediate, however far down: a saturated link helps nobody gradually.
  assert.equal(advanceQuality('full', 'video-off', 0, 1, RAMP_INTERVAL_MS), 'video-off');
  assert.equal(advanceQuality('bitrate-capped', 'video-off', 0, 1, RAMP_INTERVAL_MS), 'video-off');
  // Up is one rung per interval since the link last moved — a jump is the oscillation the model
  // exists to prevent.
  assert.equal(advanceQuality('video-off', 'full', 1_000, 3_999, RAMP_INTERVAL_MS), 'video-off');
  assert.equal(
    advanceQuality('video-off', 'full', 1_000, 4_000, RAMP_INTERVAL_MS),
    'frame-rate-lowered',
  );
  assert.equal(
    advanceQuality('frame-rate-lowered', 'full', 4_000, 6_999, RAMP_INTERVAL_MS),
    'frame-rate-lowered',
  );
  assert.equal(
    advanceQuality('frame-rate-lowered', 'full', 4_000, 7_000, RAMP_INTERVAL_MS),
    'resolution-lowered',
  );
  // A climb that has waited its interval lands on the rung above — and when the target is that
  // rung, the climb and the answer are the same thing.
  assert.equal(
    advanceQuality('resolution-lowered', 'bitrate-capped', 0, RAMP_INTERVAL_MS, RAMP_INTERVAL_MS),
    'bitrate-capped',
  );
});

test('each rung shapes the sender with the crate’s own caps', () => {
  assert.deepEqual(videoSenderParams('full', 1000), { enabled: true });
  assert.deepEqual(videoSenderParams('bitrate-capped', 1000), {
    enabled: true,
    maxBitrate: 600, // 60% of what the link measured.
  });
  assert.deepEqual(videoSenderParams('resolution-lowered', 1000), {
    enabled: true,
    maxBitrate: 600,
    scaleResolutionDownBy: 2,
  });
  assert.deepEqual(videoSenderParams('frame-rate-lowered', 1000), {
    enabled: true,
    maxBitrate: 400, // 40% once frame rate has been sacrificed.
    scaleResolutionDownBy: 2,
    maxFramerate: 15, // half of a 30 fps sender — the crate strides by sequence; a sender halves.
  });
  assert.deepEqual(videoSenderParams('video-off', 1000), { enabled: false });
});

test('a shared screen is what goes on the wire, and the camera is what comes back', () => {
  // A stream stand-in: the only thing either caller reads off one is its video track.
  const stream = (id: string): MediaStream =>
    ({ id, getVideoTracks: () => [{ id: `${id}-track` }] }) as unknown as MediaStream;
  const camera = stream('cam');
  const screen = stream('screen');

  assert.equal(videoTrackToSend(null, camera)?.id, 'cam-track', 'no share, the camera sends');
  assert.equal(videoTrackToSend(screen, camera)?.id, 'screen-track', 'a share replaces it');
  assert.equal(
    videoTrackToSend(screen, null)?.id,
    'screen-track',
    'a camera that was never acquired does not stop a share',
  );
  // No camera and no share is no video: the null is the answer, not an empty track, because
  // `shapeVideoSender` reads a null track as "send nothing" — which is what such a device does.
  assert.equal(videoTrackToSend(null, null), null);
  // A stream with no video track at all is the same fact as no stream: the audio-only case.
  const audioOnly = { getVideoTracks: () => [] } as unknown as MediaStream;
  assert.equal(videoTrackToSend(null, audioOnly), null);
  assert.equal(
    videoTrackToSend(audioOnly, camera)?.id,
    'cam-track',
    'a share with no video falls back',
  );
});

test('the delta between two readings is the only honest loss and drop percentage', () => {
  const prev: RawLinkCounters = {
    at: 0,
    packetsLost: 0,
    packetsReceived: 1_000,
    framesReceived: 300,
    framesDropped: 0,
    bytesSent: 50_000,
    rttMs: 220,
    jitterMs: 10,
    availableKbps: null,
  };
  const next: RawLinkCounters = {
    at: 1_000,
    packetsLost: 100,
    packetsReceived: 2_900,
    framesReceived: 540,
    framesDropped: 10,
    bytesSent: 175_000,
    rttMs: 220,
    jitterMs: 10,
    availableKbps: null,
  };
  const stats = linkStatsBetween(prev, next);
  assert.ok(stats !== null);
  assert.equal(stats.packetLossPct, 5, '100 lost of 100+1900 received');
  assert.equal(stats.droppedFramePct, 4, '10 dropped of 240+10 frames');
  assert.equal(stats.sentKbps, 1_000, '125000 bytes over one second');
  assert.equal(
    stats.availableKbps,
    1_000,
    'no estimate reported: available equals sent, deficit zero',
  );
  assert.equal(stats.rttMs, 220);
  assert.equal(stats.jitterMs, 10);
  // A stretch with no time in it says nothing, and a stretch with no packets says nothing.
  assert.equal(linkStatsBetween(prev, { ...prev, at: 0 }), null);
  assert.equal(linkStatsBetween(prev, { ...prev, at: 500 }), null);
  // An estimate, where the browser reports one, is read; a deficit then counts.
  const estimated = linkStatsBetween(prev, { ...next, availableKbps: 500 });
  assert.ok(estimated !== null);
  assert.equal(estimated.availableKbps, 500);
});

test('a peer connection’s counters are read from the reports a browser actually gives', async () => {
  const reports = new Map<string, object>([
    [
      'in-1',
      {
        type: 'inbound-rtp',
        packetsLost: 12,
        packetsReceived: 1_000,
        framesReceived: 240,
        framesDropped: 6,
        jitter: 0.02, // seconds, as RTP reports them.
      },
    ],
    ['out-1', { type: 'outbound-rtp', bytesSent: 125_000 }],
    [
      'pair-1',
      {
        type: 'candidate-pair',
        state: 'succeeded',
        nominated: true,
        currentRoundTripTime: 0.15,
        availableOutgoingBitrate: 250_000,
      },
    ],
    // A failed pair and an unnominated one are not the link's reading.
    [
      'pair-2',
      { type: 'candidate-pair', state: 'failed', nominated: true, currentRoundTripTime: 0.9 },
    ],
    [
      'pair-3',
      { type: 'candidate-pair', state: 'succeeded', nominated: false, currentRoundTripTime: 0.9 },
    ],
  ]);
  const pc = {
    getStats: () => Promise.resolve(reports),
  } as unknown as RTCPeerConnection;
  const counters = await readLinkCounters(pc);
  assert.ok(counters !== null);
  assert.equal(counters.packetsLost, 12);
  assert.equal(counters.packetsReceived, 1_000);
  assert.equal(counters.jitterMs, 20, 'jitter crosses from seconds to milliseconds');
  assert.equal(counters.rttMs, 150, 'the nominated, succeeded pair is the one that is read');
  assert.equal(
    counters.availableKbps,
    250,
    'the de-facto bandwidth estimate surface, read where it exists',
  );
  assert.equal(counters.bytesSent, 125_000);
  // A connection reporting nothing yet is honestly null, not zeros.
  const empty = { getStats: () => Promise.resolve(new Map()) } as unknown as RTCPeerConnection;
  assert.equal(await readLinkCounters(empty), null);
});

// --- the dialing rule -------------------------------------------------------------------------

test('the seat that joined later dials; the earlier seat answers', () => {
  const seats = [SEAT_A, SEAT_B, SEAT_C];
  // The projection is shared: every client computes the same answer from the same order.
  assert.equal(
    dialsRemote(seats, { accountId: ACC_A }, DEV_B),
    false,
    'the first seat dials nobody',
  );
  assert.equal(
    dialsRemote(seats, { accountId: ACC_B }, DEV_A),
    true,
    'the second seat dials the first',
  );
  assert.equal(dialsRemote(seats, { accountId: ACC_B }, DEV_C), false, 'but not the third');
  assert.equal(dialsRemote(seats, { accountId: ACC_C }, DEV_A), true);
  assert.equal(
    dialsRemote(seats, { accountId: ACC_C }, DEV_B),
    true,
    'a mid-call joiner dials everyone',
  );
  // A peer outside the projection is not ours to dial, and a device with no seat dials nobody.
  assert.equal(dialsRemote(seats, { accountId: ACC_A }, 'dev-x' as Id), false);
  assert.equal(dialsRemote([SEAT_B, SEAT_C], { accountId: ACC_A }, DEV_B), false);
});

test('glare breaks by device id, so exactly one side keeps its offer', () => {
  assert.equal(iKeepMyOffer('dev-a' as Id, 'dev-b' as Id), true);
  assert.equal(iKeepMyOffer('dev-b' as Id, 'dev-a' as Id), false);
  assert.notEqual(
    iKeepMyOffer(DEV_A, DEV_B),
    iKeepMyOffer(DEV_B, DEV_A),
    'the two sides of a glare must never both keep their offers',
  );
});

test('the video limit refuses the ninth stream, never the ninth participant', () => {
  assert.equal(MAX_ACTIVE_VIDEO_STREAMS, 8, 'the product limit is the crate’s own');
  assert.equal(videoAdmitted(0, true), true);
  assert.equal(videoAdmitted(7, true), true, 'the eighth stream is admitted');
  assert.equal(videoAdmitted(8, true), false, 'the ninth is refused — as a stream');
  assert.equal(videoAdmitted(8, false), false, 'a seat that wants no video is not “refused”');
});

test('the ICE servers are the join reply’s TURN relays, then the public STUN fallback', () => {
  assert.deepEqual(groupIceServers([]), [{ urls: 'stun:stun.l.google.com:19302' }]);
  const servers = groupIceServers([
    { url: 'turn:turn.example:3478', username: 'u', credential: 'c', ttlSeconds: 60, region: 'eu' },
  ]);
  assert.deepEqual(servers, [
    { urls: 'turn:turn.example:3478', username: 'u', credential: 'c' },
    { urls: 'stun:stun.l.google.com:19302' },
  ]);
});

// --- the fake transport: what the E2E tests run over ------------------------------------------

/** A local track: `enabled` is the mute surface, `stopped` is the teardown surface. */
class FakeTrack {
  enabled = true;
  stopped = false;
  constructor(readonly kind: 'audio' | 'video') {}
  stop(): void {
    this.stopped = true;
  }
}

/** A local or remote stream: the tracks the roster screen and the toggles read. */
class FakeStream {
  readonly tracks: FakeTrack[];
  constructor(kinds: Array<'audio' | 'video'>) {
    this.tracks = kinds.map((kind) => new FakeTrack(kind));
  }
  getTracks(): FakeTrack[] {
    return [...this.tracks];
  }
  getAudioTracks(): FakeTrack[] {
    return this.tracks.filter((track) => track.kind === 'audio');
  }
  getVideoTracks(): FakeTrack[] {
    return this.tracks.filter((track) => track.kind === 'video');
  }
  removeTrack(track: FakeTrack): void {
    const at = this.tracks.indexOf(track);
    if (at >= 0) {
      this.tracks.splice(at, 1);
    }
  }
}

/** One link's sending half: the surface `#shapeVideo` and `replaceTrack` touch. */
class FakeSender {
  parameters: { encodings: Array<Record<string, number>> } = { encodings: [{}] };
  track: FakeTrack | null;
  constructor(track: FakeTrack) {
    this.track = track;
  }
  replaceTrack(track: FakeTrack | null): Promise<void> {
    this.track = track;
    return Promise.resolve();
  }
  getParameters(): { encodings: Array<Record<string, number>> } {
    return this.parameters;
  }
  setParameters(parameters: { encodings: Array<Record<string, number>> }): Promise<void> {
    this.parameters = parameters;
    return Promise.resolve();
  }
}

/**
 * One end of one link. Carries no media — the assertions are about the negotiation, the sealing,
 * and the state the plane keeps — but it is honest about ordering: descriptions are set before the
 * link connects, ICE candidates leave on a timer the way real gathering does (a macrotask, so the
 * whole offer/answer cascade — which is only microtasks — finishes first), and `syncIce` mode
 * fires them during `setLocalDescription` instead, which is how the held-ICE path gets exercised.
 */
class FakePeerConnection {
  connectionState: RTCPeerConnectionState = 'new';
  onicecandidate:
    ((event: { candidate: { toJSON(): RTCIceCandidateInit } | null }) => void) | null = null;
  ontrack: ((event: { track: FakeTrack; streams: FakeStream[] }) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  hasLocal = false;
  hasRemote = false;
  link: FakeLink | null = null;
  readonly added: Array<{ track: FakeTrack; stream: FakeStream; sender: FakeSender }> = [];
  readonly addedCandidates: RTCIceCandidateInit[] = [];
  readonly iceServers: RTCIceServer[];

  constructor(
    iceServers: RTCIceServer[],
    private readonly mesh: VirtualMesh,
    private readonly syncIce: boolean,
  ) {
    this.iceServers = iceServers;
  }

  addTrack(track: FakeTrack, stream: FakeStream): FakeSender {
    const sender = new FakeSender(track);
    this.added.push({ track, stream, sender });
    return sender;
  }

  createOffer(): Promise<{ type: 'offer'; sdp: string }> {
    return Promise.resolve({ type: 'offer', sdp: 'fake-offer' });
  }

  createAnswer(): Promise<{ type: 'answer'; sdp: string }> {
    return Promise.resolve({ type: 'answer', sdp: 'fake-answer' });
  }

  setLocalDescription(): Promise<void> {
    this.hasLocal = true;
    if (this.syncIce) {
      this.#emitIce();
    } else {
      setTimeout(() => this.#emitIce(), 0);
    }
    return Promise.resolve();
  }

  setRemoteDescription(): Promise<void> {
    this.hasRemote = true;
    if (this.link !== null) {
      this.mesh.maybeConnect(this.link);
    }
    return Promise.resolve();
  }

  addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
    this.addedCandidates.push(candidate);
    return Promise.resolve();
  }

  close(): void {
    this.connectionState = 'closed';
    // A connection that closes without ever sending steps out of the queue rather than standing in
    // front of the next send: a dial that failed to describe itself leaves no connection behind.
    this.mesh.forget(this);
    this.onconnectionstatechange?.();
  }

  getStats(): Promise<Map<string, object>> {
    // Stats come from the injected reader in these tests; a plane that reaches for the real
    // surface anyway is a plane the test wants to know about.
    return Promise.reject(new Error('stats are injected'));
  }

  /** One candidate, then gathering complete — the two events a real gatherer delivers. */
  #emitIce(): void {
    this.onicecandidate?.({
      candidate: {
        toJSON: () => ({ candidate: `candidate:1 ${this.iceServers.length}`, sdpMid: '0' }),
      },
    });
    this.onicecandidate?.({ candidate: null });
  }
}

/** One link: the two fake ends, connected once both hold both descriptions. */
interface FakeLink {
  /** The device that dialed, so a re-dial is told apart from the answer it is owed. */
  dialerDevice: Id;
  dialer: FakePeerConnection;
  answerer: FakePeerConnection | null;
  connected: boolean;
}

/**
 * The virtual wire between the planes: sealed SDP and ICE relayed device to device, exactly as the
 * server relays `CALL_SDP` and `CALL_ICE` — it cannot open what it carries, because it never sees
 * a key. `hold` queues delivery, which is how a rotation is staged mid-negotiation.
 */
class VirtualMesh {
  readonly planes = new Map<Id, GroupMediaPlane>();
  readonly keys = new Map<Id, CallKeyState>();
  readonly pcs = new Map<Id, FakePeerConnection[]>();
  /**
   * The connections built for a device that have not sent yet, oldest first. A send carries only the
   * device, never the link, so this wire has to work out which connection a description belongs to.
   * The newest one for the device is not an answer: a rotation re-dials every stranded link without
   * awaiting, so two connections are built before either send resumes. The sends resume in the order
   * the connections were built, so the oldest unsent connection is the one this send belongs to.
   */
  private readonly unsent = new Map<Id, FakePeerConnection[]>();
  private readonly links = new Map<string, FakeLink>();
  private readonly queue: Array<() => void> = [];
  hold = false;
  syncIce = false;
  /** Every sealed frame the wire carried, for the assertions that pin what the server sees. */
  readonly relayed: Array<{ from: Id; to: Id; kind: 'sdp' | 'ice'; sealed: Uint8Array }> = [];

  newPeer = (device: Id, iceServers: RTCIceServer[]): RTCPeerConnection => {
    const pc = new FakePeerConnection(iceServers, this, this.syncIce);
    const mine = this.pcs.get(device) ?? [];
    mine.push(pc);
    this.pcs.set(device, mine);
    const waiting = this.unsent.get(device) ?? [];
    waiting.push(pc);
    this.unsent.set(device, waiting);
    return pc as unknown as RTCPeerConnection;
  };

  /** The connection a send from `device` belongs to — see {@link unsent}. */
  private takeSender(device: Id): FakePeerConnection | undefined {
    const waiting = this.unsent.get(device);
    if (waiting === undefined) {
      return undefined;
    }
    const sender = waiting.shift();
    this.unsent.set(device, waiting);
    return sender;
  }

  /** Drops a connection that closed without sending, so it cannot stand in front of a later send. */
  forget(pc: FakePeerConnection): void {
    for (const [device, waiting] of this.unsent) {
      const at = waiting.indexOf(pc);
      if (at >= 0) {
        waiting.splice(at, 1);
        this.unsent.set(device, waiting);
      }
    }
  }

  sendSdp = async (from: Id, to: Id, sealed: Uint8Array): Promise<void> => {
    this.relayed.push({ from, to, kind: 'sdp', sealed });
    const sender = this.takeSender(from);
    if (this.hold) {
      this.queue.push(() => void this.deliverSdp(from, to, sealed, sender));
      return;
    }
    await this.deliverSdp(from, to, sealed, sender);
  };

  sendIce = (from: Id, to: Id, sealed: Uint8Array): Promise<void> => {
    this.relayed.push({ from, to, kind: 'ice', sealed });
    if (this.hold) {
      this.queue.push(() => this.deliverIce(from, to, sealed));
    } else {
      this.deliverIce(from, to, sealed);
    }
    return Promise.resolve();
  };

  /**
   * Lifts the hold and delivers everything it queued, in the order it was sent. Lifting it first is
   * the point: the deliveries answer each other, and an answer that stayed held would never reach
   * the dialer waiting for it.
   */
  unpause(): void {
    this.hold = false;
    while (this.queue.length > 0) {
      const deliver = this.queue.shift();
      deliver?.();
    }
  }

  private pairKey(a: Id, b: Id): string {
    return [a as string, b as string].sort().join('|');
  }

  /**
   * Delivers one description to its target. `sender` is the connection the sender built for this
   * link, captured when the send left — the wire cannot re-derive it here, because two sends to the
   * same device can be in flight at once and the newest connection then belongs to the other link.
   */
  private deliverSdp = async (
    from: Id,
    to: Id,
    sealed: Uint8Array,
    sender: FakePeerConnection | undefined,
  ): Promise<void> => {
    const target = this.planes.get(to);
    if (target === undefined) {
      return;
    }
    const key = this.pairKey(from, to);
    let link = this.links.get(key);
    if (link === undefined) {
      // The first frame of a link is its offer, so the sender is the dialer.
      assert.ok(sender !== undefined, 'an offer implies the offerer just built a peer connection');
      link = { dialerDevice: from, dialer: sender, answerer: null, connected: false };
      this.links.set(key, link);
      sender.link = link;
    } else if (link.dialerDevice === from) {
      // A re-dial after a reset: the offerer's fresh peer connection replaces the stranded one. Only
      // the dialer's own device may take the seat — an answer arriving from the other end also brings a
      // peer connection this wire has never seen, and letting it claim the dialer would strand the link
      // one description short of connecting.
      if (sender !== undefined && sender !== link.dialer && sender.link === null) {
        link.dialer = sender;
        sender.link = link;
        if (link.answerer?.connectionState === 'closed') {
          link.answerer = null;
        }
      }
    } else if (link.answerer === null && sender !== undefined && sender.link === null) {
      // The other end's first frame is its answer, and the connection it built to answer is this
      // link's answerer.
      link.answerer = sender;
      sender.link = link;
    }
    await target.onSdp({ callId: CALL, fromDevice: from, toDevice: to, sealedSdp: sealed });
    this.maybeConnect(link);
  };

  private deliverIce = (from: Id, to: Id, sealed: Uint8Array): void => {
    this.planes
      .get(to)
      ?.onIce({ callId: CALL, fromDevice: from, toDevice: to, sealedCandidates: sealed });
  };

  /** Both ends hold both descriptions: the link connects, and each end sees the other's tracks. */
  maybeConnect(link: FakeLink): void {
    if (
      link.connected ||
      link.answerer === null ||
      !link.dialer.hasLocal ||
      !link.dialer.hasRemote ||
      !link.answerer.hasLocal ||
      !link.answerer.hasRemote
    ) {
      return;
    }
    link.connected = true;
    this.#connectEnd(link.dialer, link.answerer);
    this.#connectEnd(link.answerer, link.dialer);
  }

  #connectEnd(pc: FakePeerConnection, other: FakePeerConnection): void {
    pc.connectionState = 'connected';
    pc.onconnectionstatechange?.();
    for (const added of other.added) {
      pc.ontrack?.({ track: added.track, streams: [added.stream] });
    }
  }
}

/**
 * One participant's whole stack: a real frame-key state, a plane sealing and opening through it,
 * and fake media. `readCounters` is the ladder's window into the link, scripted per test.
 */
function makeParticipant(
  mesh: VirtualMesh,
  device: Id,
  account: Id,
  opts: {
    mediaKind?: CallMediaKind;
    counterScript?: RawLinkCounters[];
    /**
     * The frame-key state this seat's plane seals and opens through. It is handed in because the
     * plane captures it when the plane is built: a state installed in `mesh.keys` afterwards is a
     * *different* key from the one the closures hold, and two seats wired that way cannot open
     * each other's frames — the negotiation dies silently and no link ever connects.
     */
    keys?: CallKeyState;
  } = {},
): { plane: GroupMediaPlane; keys: CallKeyState; links: GroupMediaLink[] } {
  const keys = opts.keys ?? mesh.keys.get(device) ?? CallKeyState.create(CALL);
  mesh.keys.set(device, keys);
  const mediaKind = opts.mediaKind ?? CallMediaKind.Audio;
  const seen: GroupMediaLink[] = [];
  const script = opts.counterScript ?? [];
  const deps: GroupMediaPlaneDeps = {
    callId: CALL,
    conversationId: CONVERSATION,
    accountId: account,
    deviceId: device,
    mediaKind,
    iceServers: groupIceServers([]),
    createPeer: (iceServers) => mesh.newPeer(device, iceServers),
    acquire: (kind) =>
      Promise.resolve(
        new FakeStream(
          kind === CallMediaKind.Video ? ['audio', 'video'] : ['audio'],
        ) as unknown as MediaStream,
      ),
    sendSdp: (to, sealed) => mesh.sendSdp(device, to, sealed),
    sendIce: (to, sealed) => mesh.sendIce(device, to, sealed),
    seal: (frame) => keys.sealFrame(frame),
    open: (sealed) => keys.openFrame(sealed),
    onLinks: (links) => {
      seen.splice(0, seen.length, ...links);
    },
    iceLingerMs: 1,
    readCounters: () => Promise.resolve(script.shift() ?? scriptFallback),
  };
  const plane = new GroupMediaPlane(deps);
  mesh.planes.set(device, plane);
  return { plane, keys, links: seen };
}

/** The last scripted reading repeats, so a tick past the script's end is a tick of the same link. */
const scriptFallback: RawLinkCounters = {
  at: 0,
  packetsLost: 0,
  packetsReceived: 0,
  framesReceived: 0,
  framesDropped: 0,
  bytesSent: 0,
  rttMs: 0,
  jitterMs: 0,
  availableKbps: null,
};

/** Lets the fake's timer-fired ICE (a macrotask) land, and the linger batches flush. */
async function settle(ms = 25): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/** Two participants, both holding the same frame key, connected end to end. */
async function twoParties(opts: { syncIce?: boolean } = {}): Promise<{
  mesh: VirtualMesh;
  a: ReturnType<typeof makeParticipant>;
  b: ReturnType<typeof makeParticipant>;
}> {
  const mesh = new VirtualMesh();
  mesh.syncIce = opts.syncIce === true;
  // The first seat mints the key; the joiner receives it through the real join distribution, the
  // same sealed envelope the SDK's key domain sends over a pairwise session secret.
  const aKeys = CallKeyState.create(CALL);
  const sessionSecret = new Uint8Array(32).fill(7);
  const bKeys = CallKeyState.fromJoinDistribution(
    sessionSecret,
    CALL,
    aKeys.sealedJoinDistribution(sessionSecret),
  );
  const a = makeParticipant(mesh, DEV_A, ACC_A, { keys: aKeys });
  const b = makeParticipant(mesh, DEV_B, ACC_B, { keys: bKeys });
  await a.plane.begin([SEAT_A]);
  a.plane.seatsChanged([SEAT_A, SEAT_B]);
  await b.plane.begin([SEAT_A, SEAT_B]);
  await settle();
  return { mesh, a, b };
}

// --- two participants, end to end -------------------------------------------------------------

test('two participants negotiate one link, sealed under the frame key, and hear each other', async () => {
  const { mesh, a, b } = await twoParties();

  // The later seat dialed; the earlier seat answered — one link each, connected, with the other's
  // audio flowing as a remote stream.
  assert.deepEqual(
    a.links.map((link) => ({ phase: link.phase, audio: link.video === false })),
    [{ phase: 'connected', audio: true }],
  );
  assert.deepEqual(
    b.links.map((link) => link.phase),
    ['connected'],
  );
  const remoteAtA = a.plane.remoteStreamOf(DEV_B);
  assert.ok(remoteAtA !== null, 'the answerer holds the dialer’s stream once the link connects');
  assert.equal((remoteAtA as unknown as FakeStream).getAudioTracks().length, 1);

  // Every SDP and ICE frame the wire carried was sealed: none of them is the plain description.
  for (const frame of mesh.relayed) {
    const text = new TextDecoder().decode(frame.sealed);
    assert.ok(
      !text.includes('fake-offer') && !text.includes('fake-answer'),
      'the relay sees ciphertext',
    );
  }
  // And they truly were sealed under the frame key: the epoch's binding opens them.
  const anSdp = mesh.relayed.find((frame) => frame.kind === 'sdp');
  assert.ok(anSdp !== undefined);
  assert.doesNotThrow(() => b.keys.openFrame(anSdp.sealed));

  // The dialer's peer connection was built on the call's ICE servers (TURN list plus fallback).
  const bPcs = mesh.pcs.get(DEV_B) ?? [];
  assert.equal(bPcs.length, 1);
  assert.deepEqual(bPcs[0]?.iceServers, [{ urls: 'stun:stun.l.google.com:19302' }]);
});

test('the answerer’s candidates are held until the dialer’s remote description exists', async () => {
  // Sync-ICE mode makes the answerer gather during its setLocalDescription — before its answer
  // reaches the dialer — so the dialer must hold the candidates, not drop them.
  const { mesh, b } = await twoParties({ syncIce: true });
  const bPcs = mesh.pcs.get(DEV_B) ?? [];
  assert.equal(bPcs.length, 1);
  assert.ok(bPcs[0]?.hasRemote, 'the dialer holds the answer as its remote description');
  assert.equal(
    bPcs[0]?.addedCandidates.length,
    1,
    'the held candidates were applied once the description they follow landed',
  );
  assert.deepEqual(
    b.links.map((link) => link.phase),
    ['connected'],
  );
});

test('mute touches the microphone everywhere; the camera toggle exists only where a camera does', async () => {
  const mesh = new VirtualMesh();
  const aKeys = CallKeyState.create(CALL);
  const secret = new Uint8Array(32).fill(7);
  const bKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    aKeys.sealedJoinDistribution(secret),
  );
  const a = makeParticipant(mesh, DEV_A, ACC_A, {
    mediaKind: CallMediaKind.Video,
    keys: aKeys,
  });
  const b = makeParticipant(mesh, DEV_B, ACC_B, {
    mediaKind: CallMediaKind.Video,
    keys: bKeys,
  });
  await a.plane.begin([SEAT_A]);
  a.plane.seatsChanged([SEAT_A, SEAT_B]);
  await b.plane.begin([SEAT_A, SEAT_B]);
  await settle();

  // Both seats publish video (a two-seat roster is under the limit).
  assert.equal(a.plane.videoPublished, true);
  assert.equal(b.plane.videoPublished, true);

  // Mute is one fact, applied to the one local microphone.
  assert.equal(a.plane.toggleMute(), true);
  const localAtA = a.plane.localStream as unknown as FakeStream;
  assert.equal(localAtA.getAudioTracks()[0]?.enabled, false);
  assert.equal(a.plane.muted, true);
  assert.equal(a.plane.toggleMute(), false);
  assert.equal(localAtA.getAudioTracks()[0]?.enabled, true);

  // The camera toggles off without touching the mic, and back on.
  assert.equal(a.plane.toggleCamera(), false);
  assert.equal(localAtA.getVideoTracks()[0]?.enabled, false);
  assert.equal(a.plane.cameraOn, false);
  assert.equal(a.plane.toggleCamera(), true);
  assert.equal(localAtA.getVideoTracks()[0]?.enabled, true);

  // An audio seat has no camera toggle to give: null, not a button that does nothing.
  const meshAudio = new VirtualMesh();
  const cKeys = CallKeyState.create(CALL);
  const dKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    cKeys.sealedJoinDistribution(secret),
  );
  const c = makeParticipant(meshAudio, 'dev-c2' as Id, 'acc-c2' as Id, { keys: cKeys });
  const d = makeParticipant(meshAudio, 'dev-d2' as Id, 'acc-d2' as Id, { keys: dKeys });
  const SEAT_C2 = { userId: 'acc-c2' as Id, deviceId: 'dev-c2' as Id };
  const SEAT_D2 = { userId: 'acc-d2' as Id, deviceId: 'dev-d2' as Id };
  await c.plane.begin([SEAT_C2]);
  c.plane.seatsChanged([SEAT_C2, SEAT_D2]);
  await d.plane.begin([SEAT_C2, SEAT_D2]);
  await settle();
  assert.equal(c.plane.videoPublished, false);
  assert.equal(c.plane.toggleCamera(), null);
  assert.equal(c.plane.cameraOn, null);
});

test('a video seat joins a roster the limit has filled as audio, and says so', async () => {
  const mesh = new VirtualMesh();
  const keys = CallKeyState.create(CALL);
  mesh.keys.set(DEV_A, keys);
  // A roster of eight remote seats (the limit's worth) and this seat's own: the video the seat
  // asked for is refused as a stream, and the seat stays.
  const seats = [SEAT_A, SEAT_B, SEAT_C];
  for (let i = 0; i < MAX_ACTIVE_VIDEO_STREAMS - 2; i++) {
    seats.push({ userId: `acc-${i}` as Id, deviceId: `dev-${i}` as Id });
  }
  const a = makeParticipant(mesh, DEV_A, ACC_A, { mediaKind: CallMediaKind.Video });
  await a.plane.begin(seats);
  assert.equal(a.plane.videoPublished, false, 'the ninth stream is refused');
  assert.equal(a.plane.cameraOn, null);
  const local = a.plane.localStream as unknown as FakeStream;
  assert.equal(local.getVideoTracks().length, 0, 'the camera track was never kept');
  assert.equal(local.getAudioTracks().length, 1, 'the microphone was');
  assert.deepEqual(
    a.links.map((link) => link.phase),
    [],
    'a first seat dials nobody — and refused video never refused the seat',
  );
});

test('a microphone that cannot be acquired is stated as a failure, not faked', async () => {
  const mesh = new VirtualMesh();
  mesh.keys.set(DEV_A, CallKeyState.create(CALL));
  const failures: string[] = [];
  const plane = new GroupMediaPlane({
    callId: CALL,
    conversationId: CONVERSATION,
    accountId: ACC_A,
    deviceId: DEV_A,
    mediaKind: CallMediaKind.Audio,
    iceServers: groupIceServers([]),
    createPeer: (iceServers) => mesh.newPeer(DEV_A, iceServers),
    acquire: () => Promise.reject(new Error('no microphone')),
    sendSdp: () => Promise.resolve(),
    sendIce: () => Promise.resolve(),
    seal: (frame) => mesh.keys.get(DEV_A)!.sealFrame(frame),
    open: (sealed) => mesh.keys.get(DEV_A)!.openFrame(sealed),
    onFailure: (what) => failures.push(what),
  });
  mesh.planes.set(DEV_A, plane);
  await plane.begin([SEAT_A, SEAT_B]);
  assert.deepEqual(failures, ['Microphone unavailable. Check permissions and try again.']);
  assert.equal(plane.localStream, null);
  assert.deepEqual(
    mesh.pcs.get(DEV_A) ?? [],
    [],
    'no link is built around a microphone that does not exist',
  );
});

test('leave closes every link and stops every local track; a departure closes exactly its own', async () => {
  const { mesh, a, b } = await twoParties();
  // A third seat joins and dials both, exactly as the join order says.
  const secret = new Uint8Array(32).fill(7);
  const cKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    mesh.keys.get(DEV_A)!.sealedJoinDistribution(secret),
  );
  const c = makeParticipant(mesh, DEV_C, ACC_C, { keys: cKeys });
  a.plane.seatsChanged([SEAT_A, SEAT_B, SEAT_C]);
  b.plane.seatsChanged([SEAT_A, SEAT_B, SEAT_C]);
  await c.plane.begin([SEAT_A, SEAT_B, SEAT_C]);
  await settle();

  // The full mesh: every seat holds two connected links.
  for (const [name, participant] of [
    ['a', a],
    ['b', b],
    ['c', c],
  ] as const) {
    assert.deepEqual(
      participant.links.map((link) => link.phase),
      ['connected', 'connected'],
      `${name} is connected to both other seats`,
    );
  }

  // C leaves: its own links close and its tracks stop; A and B close exactly their link to C.
  c.plane.leave();
  assert.deepEqual(
    c.links.map((link) => link.phase),
    ['closed', 'closed'],
  );
  const cLocal = c.plane.localStream;
  assert.equal(cLocal, null, 'leave tears the local media down');
  const aPcs = mesh.pcs.get(DEV_A) ?? [];
  const bPcs = mesh.pcs.get(DEV_B) ?? [];
  a.plane.seatsChanged([SEAT_A, SEAT_B]);
  b.plane.seatsChanged([SEAT_A, SEAT_B]);
  assert.equal(
    aPcs.filter((pc) => pc.connectionState === 'closed').length,
    1,
    'A closed its link to C',
  );
  assert.equal(
    bPcs.filter((pc) => pc.connectionState === 'closed').length,
    1,
    'B closed its link to C',
  );
  assert.deepEqual(
    a.links.map((link) => link.phase),
    ['connected'],
    'A’s link to B survived C’s departure',
  );
});

test('a rotation strands in-flight negotiations; the dialer re-dials under the fresh key', async () => {
  const { mesh, a, b } = await twoParties();
  // C joins, but the wire holds everything: C's offers sit sealed under epoch 0, mid-negotiation.
  const secret = new Uint8Array(32).fill(7);
  const cKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    mesh.keys.get(DEV_A)!.sealedJoinDistribution(secret),
  );
  const c = makeParticipant(mesh, DEV_C, ACC_C, { keys: cKeys });
  a.plane.seatsChanged([SEAT_A, SEAT_B, SEAT_C]);
  b.plane.seatsChanged([SEAT_A, SEAT_B, SEAT_C]);
  mesh.hold = true;
  await c.plane.begin([SEAT_A, SEAT_B, SEAT_C]);
  assert.deepEqual(
    c.links.map((link) => link.phase),
    ['connecting', 'connecting'],
    'C’s links are mid-negotiation, their offers still on the wire',
  );

  // The roster moved again: the first seat rotates, and everyone adopts epoch 1.
  const sealedUpdate = mesh.keys.get(DEV_A)!.rotate();
  mesh.keys.get(DEV_B)!.adopt(1, sealedUpdate);
  cKeys.adopt(1, sealedUpdate);

  // The epoch change resets C's unconnected links and re-dials under the fresh key.
  c.plane.keyEpochChanged();
  const cPcs = mesh.pcs.get(DEV_C) ?? [];
  assert.equal(cPcs.length, 4, 'two stranded links closed, two fresh ones built');
  assert.equal(cPcs.filter((pc) => pc.connectionState === 'closed').length, 2);

  // The wire unpauses: the epoch-0 offers arrive first and are unopenable — dropped silently —
  // then the epoch-1 offers land, are answered, and connect.
  mesh.unpause();
  await settle();
  assert.deepEqual(
    c.links.map((link) => link.phase),
    ['connected', 'connected'],
  );
  assert.deepEqual(
    a.links.map((link) => link.phase),
    ['connected', 'connected'],
    'A’s connected link to B survived the rotation, and C’s fresh dial reached it',
  );
  assert.deepEqual(
    b.links.map((link) => link.phase),
    ['connected', 'connected'],
  );
});

// --- the ladder over a live link --------------------------------------------------------------

test('a live video link degrades to its target and recovers one rung per interval', async () => {
  const mesh = new VirtualMesh();
  const aKeys = CallKeyState.create(CALL);
  const secret = new Uint8Array(32).fill(7);
  const bKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    aKeys.sealedJoinDistribution(secret),
  );
  // The counter script, one reading per tick: a baseline, a bad window (score 34 →
  // resolution-lowered), then two clean windows (score 0 → full).
  const c0: RawLinkCounters = {
    at: 0,
    packetsLost: 0,
    packetsReceived: 1_000,
    framesReceived: 300,
    framesDropped: 0,
    bytesSent: 50_000,
    rttMs: 220,
    jitterMs: 10,
    availableKbps: null,
  };
  const c1: RawLinkCounters = {
    at: 1_000,
    packetsLost: 100,
    packetsReceived: 2_900,
    framesReceived: 540,
    framesDropped: 10,
    bytesSent: 175_000,
    rttMs: 220,
    jitterMs: 10,
    availableKbps: null,
  };
  const c2: RawLinkCounters = {
    at: 4_000,
    packetsLost: 100,
    packetsReceived: 4_900,
    framesReceived: 780,
    framesDropped: 10,
    bytesSent: 275_000,
    rttMs: 90,
    jitterMs: 2,
    availableKbps: 5_000,
  };
  const c3: RawLinkCounters = {
    at: 7_000,
    packetsLost: 100,
    packetsReceived: 6_900,
    framesReceived: 1_020,
    framesDropped: 10,
    bytesSent: 375_000,
    rttMs: 90,
    jitterMs: 2,
    availableKbps: 5_000,
  };
  const a = makeParticipant(mesh, DEV_A, ACC_A, {
    mediaKind: CallMediaKind.Video,
    counterScript: [c0, c1, c2, c3],
    keys: aKeys,
  });
  const b = makeParticipant(mesh, DEV_B, ACC_B, { keys: bKeys });
  await a.plane.begin([SEAT_A]);
  a.plane.seatsChanged([SEAT_A, SEAT_B]);
  await b.plane.begin([SEAT_A, SEAT_B]);
  await settle();
  assert.equal(a.plane.videoPublished, true);

  // The baseline reading establishes the window; nothing moves yet.
  await a.plane.tick(10_000);
  assert.equal(a.links[0]?.quality, 'full');

  // The bad window: score 34, target resolution-lowered, landed on at once, and the sender shaped.
  await a.plane.tick(11_000);
  assert.equal(a.links[0]?.quality, 'resolution-lowered');
  const aPcs = mesh.pcs.get(DEV_A) ?? [];
  const videoSender = aPcs[0]?.added.find((added) => added.track.kind === 'video')?.sender;
  assert.ok(videoSender !== undefined, 'the video sender exists on the connected link');
  assert.equal(
    videoSender.parameters.encodings[0]?.maxBitrate,
    600_000,
    '60% of the measured 1000 kbps',
  );
  assert.equal(videoSender.parameters.encodings[0]?.scaleResolutionDownBy, 2);

  // The clean window: recovery climbs one rung, not to the top.
  await a.plane.tick(16_000);
  assert.equal(a.links[0]?.quality, 'bitrate-capped');

  // Another clean window, one interval later: the top rung.
  await a.plane.tick(19_000);
  assert.equal(a.links[0]?.quality, 'full');

  // The audio never left the ladder's decisions: the mic track was never touched.
  const localAtA = a.plane.localStream as unknown as FakeStream;
  assert.equal(localAtA.getAudioTracks()[0]?.enabled, true);
});

// --- the frame key's own arithmetic, pinned where the plane depends on it ---------------------

test('a frame sealed under one epoch does not open under the next, until the update is adopted', () => {
  const aKeys = CallKeyState.create(CALL);
  const secret = new Uint8Array(32).fill(7);
  const bKeys = CallKeyState.fromJoinDistribution(
    secret,
    CALL,
    aKeys.sealedJoinDistribution(secret),
  );
  const frame = new TextEncoder().encode('an sdp description');
  const sealed = aKeys.sealFrame(frame);
  assert.doesNotThrow(
    () => bKeys.openFrame(sealed),
    'the joiner holds the key the offer was sealed under',
  );

  const sealedUpdate = aKeys.rotate();
  const resealed = aKeys.sealFrame(frame);
  assert.throws(
    () => bKeys.openFrame(resealed),
    'a frame from after the rotation is not the joiner’s to open',
  );
  bKeys.adopt(1, sealedUpdate);
  assert.doesNotThrow(
    () => bKeys.openFrame(resealed),
    'the distributed update is exactly what re-opens it',
  );
  assert.throws(() => bKeys.adopt(1, sealedUpdate), 'a replayed update is refused');
});
