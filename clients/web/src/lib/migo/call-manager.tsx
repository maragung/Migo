'use client';

/**
 * The call manager: one React context that owns a device's single live call.
 *
 * The SDK's calls domain is pure signaling — it sends what it is handed and delivers what arrives.
 * This provider is the piece above it that a call UI actually needs: it drives a WebRTC peer
 * connection in step with the signaling (offer on invite, answer on accept, candidates relaying in
 * both directions), projects the tracked {@link ActiveCall} the overlay renders, and owns the
 * teardown so no path — hang up, decline, cancel, expiry, a dropped session — leaves a microphone
 * open or a peer connection half-alive.
 *
 * # Why the caller cannot send ICE until the answer arrives
 *
 * An invite names the callee *account*, and the server rings every device on it; which device
 * answers is not knowable in advance. The answering device names itself in the answer relay's
 * `fromDevice`, so until that arrives a caller's gathered candidates have nowhere to go — they
 * batch here, and flush the moment the answer lands. A callee has no such wait: the invite event
 * already carried `callerDevice`.
 *
 * # The reconnect window
 *
 * A transport blip must not end a call (section 180): the peer connection going `disconnected` —
 * or `failed`, which is ICE's verdict on the current candidate pairs rather than on the call —
 * shows *Reconnecting* and starts a window; media coming back cancels it, the window expiring ends
 * the call with `Network` — and, since the server and the peer still think the call is live, fires
 * a best-effort `CALL_END` so the other side is spared the whole window. Inside the window the
 * caller offers an ICE restart through `CALL_RENEGOTIATE` (see {@link restartIce}), which is what
 * makes the window a recovery attempt rather than only a grace period: a blip that moved the two
 * devices' addresses leaves every old candidate pair dead, and only a fresh set can bring the media
 * back.
 *
 * # The ring's lifecycle
 *
 * Three facts keep a ring honest. Invites are Critical frames, delivered at least once, so a
 * redelivered invite names a call this device already knows — it is ignored, never declined, or
 * the decline would hang up the very ring it re-announces. An unanswered invite ends itself at
 * `expiresAt`; both sides arm a local mirror of that deadline — the caller's so its "Calling…"
 * screen never outlives the invite, the callee's so a ring never outlives it either — even if
 * the server's `Ended` event is late or lost. And an event for the call still ringing inbound
 * retires the ring with a note: an `Ended` says the caller gave up (a missed call), while a
 * `Connecting` or `Connected` says a sibling device answered — the call moved, it was not
 * missed — because a screen that keeps ringing a dead call teaches its user to distrust every
 * ring after it.
 *
 * # The call key
 *
 * Every SDP and ICE blob this manager sends is sealed by {@link sealCallSignal} under a per-call
 * key, and the server relays bytes it cannot read (§165). The key is minted here, by the caller,
 * and reaches the conversation inside the E2EE message layer — a control event sent through the
 * messaging domain *before* the invite, so every device that may answer holds the key by the time
 * the ring arrives. The callee's side of that contract is the accept path's wait: a key that has
 * not landed yet (the frames crossed, briefly) is waited for, not failed over.
 *
 * The key lives in a ref map for the session's calls and is forgotten when its call ends. Media
 * content is never logged here — not the SDP, not the candidates, not the streams; failures are
 * recorded as facts ("could not start the call"), not payloads. The key joins that rule: it is
 * handed to the seal and shown nowhere else.
 *
 * Media content is never logged here — not the SDP, not the candidates, not the streams; failures
 * are recorded as facts ("could not start the call"), not payloads.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import {
  CallDeclineReason,
  CallEndReason,
  CallMediaKind,
  CallState,
  ContentType,
  newId,
} from '@migo/sdk';
import type {
  ActiveCall,
  CallInviteEvent,
  CallStateEvent,
  CallSdp,
  CallIce,
  Id,
  TurnServer,
} from '@migo/sdk';

import {
  CALL_KEY_EVENT,
  INVITE_RINGING,
  answersRingingCall,
  callEndReasonOf,
  callMediaKindOf,
  callStateOf,
  decodeCallKeyEvent,
  decodeIceBatch,
  decodeSdpDescription,
  encodeCallKeyEvent,
  endsRingingCall,
  encodeIceBatch,
  encodeSdpDescription,
  generateCallKey,
  incomingInviteDisposition,
  inviteEndReason,
  openCallSignal,
  ringTimeoutMs,
  sdpDisposition,
  sealCallSignal,
  SdpDisposition,
} from './call-signal.js';
import type { SdpDescription } from './call-signal.js';
import {
  QUALITY_POLL_MS,
  advanceMeasurement,
  cappedQuality,
  degradedAt,
  qualityReport,
} from './call-quality.js';
import {
  callAudioConstraints,
  callVideoConstraints,
  cameraDevicesOf,
  canSelectOutput,
  outputDevicesOf,
  switchCameraId,
} from './call-devices.js';
import type { CallDevice } from './call-devices.js';
import {
  readLinkCounters,
  shapeAudioSender,
  shapeVideoSender,
  videoTrackToSend,
} from './group-media.js';
import type { LinkQuality, LinkStats, RawLinkCounters } from './group-media.js';
import { useMigo } from './use-migo.js';

/** How long gathered ICE candidates linger before one relay carries them (section 165: batch, briefly). */
const ICE_LINGER_MS = 250;
/** How long a disconnected transport gets before the call ends as a network failure. */
const RECONNECT_WINDOW_MS = 30_000;
/**
 * How long an accept waits for the call's key before giving up on answering.
 *
 * The caller sends the key message and *then* the invite, so in the ordinary case the key is
 * already here when the user clicks accept — the wait is for the frames crossing on a slow
 * connection, not for the common path. Five seconds is far under the invite's own expiry and far
 * over any honest reordering.
 */
const CALL_KEY_WAIT_MS = 5_000;
/**
 * What the missed-call note says when an inbound ring retires because the call ended before it
 * was answered. Exported so the overlay can label its card with the same fact the manager states.
 */
export const MISSED_CALL_MESSAGE = 'Missed call';

/**
 * What the note says when an inbound ring retires because a sibling device answered: the call
 * was not missed, it moved — and a screen that says "missed" for a call being spoken on
 * elsewhere sends its user to the phone that is already in the conversation.
 */
export const ANSWERED_ELSEWHERE_MESSAGE = 'Answered on another device';

/**
 * The slice of the client the ICE-server resolution needs, so a caller (or a test) can supply
 * any object with this one method rather than a whole {@link MigoClient}.
 */
export interface TurnClient {
  readonly calls: {
    getTurnServers(callId: Id): Promise<TurnServer[]>;
  };
}

/**
 * The public STUN fallback every peer connection carries.
 *
 * The server's TURN list comes from configuration and may legitimately be empty (relay off,
 * direct connections only); a STUN server costs nothing and is what lets a direct connection
 * find its public reflexive address at all, so without it calls work only on the same LAN.
 */
const STUN_FALLBACK: RTCIceServer = { urls: 'stun:stun.l.google.com:19302' };

/**
 * The media an answer falls back to when the invited kind is not there to take: a video answer
 * whose camera cannot be acquired retries as audio, and only a microphone that fails too is a
 * failure. Pure over the acquisition so a test can pin the decision without a device.
 */
export async function answerMediaWithFallback(
  mediaKind: CallMediaKind,
  acquire: (kind: CallMediaKind) => Promise<MediaStream>,
): Promise<MediaStream> {
  if (mediaKind !== CallMediaKind.Video) {
    return acquire(mediaKind);
  }
  try {
    return await acquire(mediaKind);
  } catch {
    return acquire(CallMediaKind.Audio);
  }
}

/**
 * The ICE servers for one call's peer connection: the configured TURN relays, then the public
 * STUN fallback.
 *
 * A TURN fetch that fails or returns nothing still yields the fallback — a call that must relay
 * will fail to connect either way, but a call that only needed STUN must not be refused because
 * the relay list was unreachable. An entry with an empty username is an anonymous relay: the
 * credential fields stay absent rather than empty.
 */
export async function iceServersForCall(client: TurnClient, callId: Id): Promise<RTCIceServer[]> {
  const turnServers = await client.calls.getTurnServers(callId).catch(() => []);
  const iceServers: RTCIceServer[] = turnServers.map((server) => ({
    urls: server.url,
    ...(server.username !== '' ? { username: server.username } : {}),
    ...(server.credential !== '' ? { credential: server.credential } : {}),
  }));
  iceServers.push(STUN_FALLBACK);
  return iceServers;
}

/** What the rest of the app reads and calls. */
export interface CallManagerValue {
  /** The call this device is in, including one that just ended (until dismissed). */
  activeCall: ActiveCall | null;
  /** A ringing inbound call nobody has answered yet. */
  incomingCall: CallInviteEvent | null;
  /** Whether this side's microphone is muted. */
  muted: boolean;
  /**
   * Whether this side's camera is on. Always false for a voice call and for a device with no
   * camera, because there is nothing to publish and a toggle that claimed otherwise would be a
   * control that lies about what it did.
   */
  cameraOn: boolean;
  /**
   * Whether a connected call's quality has fallen far enough to pause video. False until a
   * measurement says otherwise, and false for every voice call however bad the link gets: the
   * bottom rung means this endpoint stopped sending a camera, and a voice call never was.
   */
  degraded: boolean;
  /**
   * The rung section 180's quality indicator shows, or `null` before the first measurement — a
   * call whose link has been sampled once. Null is not the same as a good rung: a screen that said
   * "Excellent" before measuring anything would be guessing, which is the one thing an indicator
   * exists not to do.
   */
  quality: LinkQuality | null;
  /**
   * The tier the user pinned the call to, or null while the ladder is on its own. A ceiling and
   * never a floor: a pinned call still descends when its link demands, because the alternative is
   * video sent into a congested path under a screen claiming a tier the call is not on.
   */
  qualityCeiling: LinkQuality | null;
  /** Whether the low-bandwidth mode is on. Section 180 asks for it on voice and video calls alike. */
  lowBandwidth: boolean;
  /** Pins the call to a tier, or returns it to automatic with null. Takes effect on the spot. */
  setQualityCeiling: (ceiling: LinkQuality | null) => void;
  /**
   * Turns the low-bandwidth mode on or off. On a video call it caps the ladder; on a voice call,
   * where there is no video to give up, it caps the audio bitrate instead.
   */
  setLowBandwidth: (on: boolean) => void;
  /**
   * Whether this device is sharing its screen right now. Section 180 requires the sharer to keep
   * seeing that it is sharing for as long as it lasts — a forgotten share is the commonest data
   * leak there is — so this is state the screen reads, not a note the manager keeps to itself.
   */
  sharingScreen: boolean;
  /**
   * The captured screen while a share runs, for the self-view. The camera is still the camera, and
   * a self-view showing the user's face through a screen share says the opposite of what is
   * happening.
   */
  screenStream: MediaStream | null;
  /**
   * The audio outputs the platform offers, empty on a browser with no output selection. Section 180
   * names speaker, earpiece, Bluetooth, and wired headset, and this is whatever the platform
   * actually reports — a list the application never invents and never pads.
   */
  outputs: CallDevice[];
  /**
   * The cameras the platform offers. Fewer than two means there is nothing to switch to, which is
   * the fact the camera-switch control is drawn from rather than its own idea of a phone.
   */
  cameras: CallDevice[];
  /** The audio output in use, or null for the platform's default. */
  outputId: string | null;
  /** This side's microphone/camera stream, for the small self-view. */
  localStream: MediaStream | null;
  /** The peer's stream once their tracks arrive, for the main view. */
  remoteStream: MediaStream | null;
  /** When the current (or just-ended) call ended, for the ended screen's duration. */
  endedAt: number | null;
  /** Why a call could not even be placed (permissions, no device), when nothing else is showing. */
  callError: string | null;
  /** Places a call: media, offer, invite. */
  startCall: (conversationId: Id, calleeId: Id, mediaKind: CallMediaKind) => Promise<void>;
  /** Answers the ringing inbound call: media, answer, and the relay that carries it. */
  acceptCall: () => Promise<void>;
  /**
   * Sends a sealed SDP answer for the inbound call: `CALL_ANSWER` tells the server the call is
   * answered, and `CALL_SDP` carries the answer itself to the caller's device.
   */
  answerCall: (sealedAnswer: Uint8Array) => Promise<void>;
  /** Declines the ringing inbound call. */
  declineCall: () => Promise<void>;
  /** Cancels the call this device placed while it still rings. */
  cancelCall: () => Promise<void>;
  /** Ends the established call with a reason. */
  endCall: (reason: CallEndReason) => Promise<void>;
  /** Mutes or unmutes this side's microphone. */
  toggleMute: () => void;
  /** Turns this side's camera on or off. Does nothing on a call with no camera to turn. */
  toggleCamera: () => void;
  /**
   * Switches to the platform's next camera. Does nothing when there is only one, which is the fact
   * the control is drawn from: a device with a single camera has nothing to switch to, and a button
   * that did nothing would be worse than no button.
   */
  switchCamera: () => Promise<void>;
  /**
   * Routes the call's audio to one of {@link outputs}, or to the platform's default when given
   * null. Takes effect on the connected media, not on the next call: section 180 asks for movement
   * between outputs while the call runs.
   */
  setOutputDevice: (deviceId: string | null) => void;
  /**
   * Starts or stops a screen share on a connected video call. Silently does nothing on a voice
   * call: section 180 makes screen sharing a video-call capability, and a voice call has no video
   * m-line for a share to ride.
   */
  toggleScreenShare: () => Promise<void>;
  /** Dismisses the ended screen (or a placement error), leaving no call tracked. */
  dismissCall: () => void;
}

const CallManagerContext = createContext<CallManagerValue | null>(null);

export function CallManagerProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId } = useMigo();

  const [activeCall, setActiveCall] = useState<ActiveCall | null>(null);
  const [incomingCall, setIncomingCall] = useState<CallInviteEvent | null>(null);
  const [muted, setMuted] = useState(false);
  const [cameraOn, setCameraOn] = useState(false);
  const [degraded, setDegraded] = useState(false);
  const [quality, setQuality] = useState<LinkQuality | null>(null);
  const [qualityCeiling, setQualityCeilingState] = useState<LinkQuality | null>(null);
  const [lowBandwidth, setLowBandwidthState] = useState(false);
  const [sharingScreen, setSharingScreen] = useState(false);
  const [screenStream, setScreenStream] = useState<MediaStream | null>(null);
  const [outputs, setOutputs] = useState<CallDevice[]>([]);
  const [cameras, setCameras] = useState<CallDevice[]>([]);
  const [outputId, setOutputId] = useState<string | null>(null);
  const [localStream, setLocalStream] = useState<MediaStream | null>(null);
  const [remoteStream, setRemoteStream] = useState<MediaStream | null>(null);
  const [endedAt, setEndedAt] = useState<number | null>(null);
  const [callError, setCallError] = useState<string | null>(null);

  // The event handlers are registered once per client, so everything they read must be a ref.
  const clientRef = useRef(client);
  clientRef.current = client;
  const accountIdRef = useRef(accountId);
  accountIdRef.current = accountId;

  const pcRef = useRef<RTCPeerConnection | null>(null);
  const activeRef = useRef<ActiveCall | null>(null);
  const incomingRef = useRef<CallInviteEvent | null>(null);
  const localStreamRef = useRef<MediaStream | null>(null);
  const mutedRef = useRef(false);
  /**
   * Whether this side's camera is on, which is the user's wish rather than the ladder's: the rung
   * decides whether any video is worth sending and this decides whether there is any to send, and
   * the two are asked separately everywhere they meet.
   */
  const cameraOnRef = useRef(false);
  /** The peer device relays are addressed to; null until the answer names it (or the invite did). */
  const peerDeviceRef = useRef<Id | null>(null);
  /** Candidates gathered but not yet relayed, waiting for the batch linger or a target device. */
  const iceBatchRef = useRef<RTCIceCandidateInit[]>([]);
  const iceTimerRef = useRef<number | null>(null);
  /** Candidates the peer relayed before this side's remote description was set, applied after it is. */
  const heldIceRef = useRef<RTCIceCandidateInit[]>([]);
  const remoteDescriptionSetRef = useRef(false);
  /**
   * Whether an offer of *this* side's is outstanding — the invite's own offer, or a restart's — and
   * whose answer has not come back.
   *
   * This is the only thing that tells the two kinds of arriving SDP apart, and it has to be a fact
   * about this side rather than about the frame: the server delivers a renegotiation to its target
   * as `CALL_SDP`, the same opcode an invite's answer arrives on, so a receiver that read the
   * frame's name would read every incoming renegotiation as the answer to an offer it never made.
   * Whichever side holds an outstanding offer reads the next SDP as its answer; the other side reads
   * it as an offer to answer.
   */
  const localOfferPendingRef = useRef(false);
  const reconnectTimerRef = useRef<number | null>(null);
  /** The caller's local mirror of the invite's expiry, while its call still rings unanswered. */
  const ringTimerRef = useRef<number | null>(null);
  /**
   * The callee's mirror of the same deadline. The incoming ring has no tracked call to land an
   * `Ended` on — before the server ran its own sweep, a caller whose client died silently left
   * this side ringing for as long as the screen cared to.
   */
  const incomingTimerRef = useRef<number | null>(null);
  /**
   * Whether a placement is between its first synchronous step and its last: the guard a second
   * click must hit *before* any await, because `activeRef` only exists once the invite replies.
   */
  const startingRef = useRef(false);
  /**
   * Whether a `CALL_END` has already gone out for the tracked call. A network death and a hang-up
   * and a closing tab all end the call locally; only the first of them should reach the server.
   */
  const endSentRef = useRef(false);
  /** When this side began setting the call up, for the one-time setupMs report. */
  const setupStartRef = useRef<number | null>(null);

  // --- the quality plane's own state: one link, sampled every QUALITY_POLL_MS ---

  /** This call's video sender, held from the moment its track was attached. */
  const videoSenderRef = useRef<RTCRtpSender | null>(null);
  /**
   * The screen this endpoint is sharing, if it is. Separate from the camera stream rather than
   * mixed into it: the camera has to survive a share (it is what comes back when one stops) and the
   * capture has to be stoppable on its own, because a screen share left running is a live window on
   * the user's desktop with nothing in the interface saying so.
   */
  const screenStreamRef = useRef<MediaStream | null>(null);
  /**
   * The camera the call is acquired from, or null for the platform's own choice.
   *
   * Held as a wish rather than read back from the track: `getUserMedia` is free to substitute when
   * the ideal device is gone, and the track does not say which one it settled on. `deviceId` from
   * the settings is read back after acquisition to keep this honest.
   */
  const cameraIdRef = useRef<string | null>(null);
  /**
   * The camera list as last read, mirrored into a ref because {@link switchCamera} is asked for the
   * next one at the moment the button is pressed, and a handler that read React state would read
   * whatever the render it closed over happened to hold.
   */
  const camerasRef = useRef<CallDevice[]>([]);
  /**
   * Whether that sender currently carries the camera. Held here rather than read back from the
   * sender, because `replaceTrack(null)` is exactly the state that makes a sender unable to report
   * what it used to carry.
   */
  const videoAttachedRef = useRef(false);
  /**
   * This call's audio sender, which the low-bandwidth mode caps and lifts.
   *
   * Kept for the same reason as the video sender: the ladder never touches audio, so the one thing
   * the mode does on a voice call — where the ladder has nothing to give up — is reach it here.
   */
  const audioSenderRef = useRef<RTCRtpSender | null>(null);
  /** The polling timer, armed when a call connects and cleared with everything else. */
  const qualityTimerRef = useRef<number | null>(null);
  /** The rung the link is on, and when it last moved — the recovery ramp is measured from the move. */
  const qualityRef = useRef<LinkQuality>('full');
  /**
   * The tier the user pinned the call to, or null for automatic. A ceiling and not a floor: the
   * ladder still descends below it when the link demands, and the value here is the user's wish
   * rather than anything the link agreed to.
   */
  const qualityCeilingRef = useRef<LinkQuality | null>(null);
  /** Whether the low-bandwidth mode is on, mirrored into a ref for the poll that reads it. */
  const lowBandwidthRef = useRef(false);
  const qualityChangedAtRef = useRef(0);
  /** The previous reading, which the next one is subtracted from; null until the first sample. */
  const qualityCountersRef = useRef<RawLinkCounters | null>(null);
  /** The most recent measurement, so a call that ends reports numbers rather than only a setup time. */
  const lastMeasureRef = useRef<{ stats: LinkStats; counters: RawLinkCounters } | null>(null);
  /**
   * The sealing key of each call this session has seen, keyed by call id. Written by the caller
   * (which minted it) and by the message listener (which adopts the caller's key event); read by
   * every seal and open. An entry leaves with its call.
   */
  const callKeysRef = useRef(new Map<Id, Uint8Array>());
  /** Accepts waiting for a call's key, resolved the moment the message listener adopts one. */
  const callKeyWaitersRef = useRef(new Map<Id, Array<(key: Uint8Array) => void>>());

  // --- tracked-state writers: ref first (handlers read it synchronously), then React state ---

  const setActive = useCallback((call: ActiveCall | null): void => {
    activeRef.current = call;
    setActiveCall(call);
  }, []);

  const setIncoming = useCallback(
    (event: CallInviteEvent | null): void => {
      if (incomingTimerRef.current !== null) {
        clearTimeout(incomingTimerRef.current);
        incomingTimerRef.current = null;
      }
      if (event !== null) {
        // The callee's mirror of the invite's expiry: the server's sweep ends the call on its
        // side, but its `Ended` can be late or lost, and a ring must never outlive the invite
        // that backs it. Firing with a *different* invite showing (or none) does nothing — the
        // timer belongs to the call it was armed for, not to whatever rings next.
        const callId = event.callId;
        incomingTimerRef.current = window.setTimeout(
          () => {
            incomingTimerRef.current = null;
            const still = incomingRef.current;
            if (still === null || still.callId !== callId) {
              return;
            }
            incomingRef.current = null;
            setIncomingCall(null);
            setCallError(MISSED_CALL_MESSAGE);
          },
          ringTimeoutMs(event.expiresAt, Date.now()),
        );
      }
      incomingRef.current = event;
      setIncomingCall(event);
    },
    [setCallError],
  );

  /** Whether a call occupies this device — an ended call still on screen does not block a new one. */
  const callInProgress = useCallback(
    (): boolean => activeRef.current !== null && activeRef.current.state !== CallState.Ended,
    [],
  );

  // --- the call key: adopt, forget, wait ---

  /**
   * Adopts a call's sealing key — minted here for a call we place, or arrived through the E2EE
   * message layer for a call we are rung for — and wakes any accept waiting on it.
   */
  const adoptCallKey = useCallback((callId: Id, key: Uint8Array): void => {
    callKeysRef.current.set(callId, key);
    const waiters = callKeyWaitersRef.current.get(callId);
    if (waiters === undefined) {
      return;
    }
    callKeyWaitersRef.current.delete(callId);
    for (const waiter of waiters) {
      waiter(key);
    }
  }, []);

  /**
   * Waits for a call's key to arrive, resolving with it — or with `null` once `timeoutMs` has
   * passed without one. The caller sends the key message before the invite, so a wait that runs
   * its full course means the frames crossed badly; the accept path treats `null` as "cannot
   * answer", exactly like any other unopenable call.
   */
  const waitForCallKey = useCallback(
    (callId: Id, timeoutMs: number): Promise<Uint8Array | null> => {
      const present = callKeysRef.current.get(callId);
      if (present !== undefined) {
        return Promise.resolve(present);
      }
      return new Promise((resolve) => {
        const waiters = callKeyWaitersRef.current.get(callId) ?? [];
        callKeyWaitersRef.current.set(callId, waiters);
        const finish = (key: Uint8Array | null): void => {
          clearTimeout(timer);
          const remaining = callKeyWaitersRef.current.get(callId);
          if (remaining !== undefined) {
            remaining.splice(remaining.indexOf(waiter), 1);
            if (remaining.length === 0) {
              callKeyWaitersRef.current.delete(callId);
            }
          }
          resolve(key);
        };
        const waiter = (key: Uint8Array): void => finish(key);
        const timer = setTimeout(() => finish(null), timeoutMs);
        waiters.push(waiter);
      });
    },
    [],
  );

  /** Forgets a call's key. Called when the call ends — the key's only job was that call. */
  const forgetCallKey = useCallback((callId: Id): void => {
    callKeysRef.current.delete(callId);
    callKeyWaitersRef.current.delete(callId);
  }, []);

  // --- teardown ---

  /** Disarms the caller's local expiry mirror; safe to call when nothing is armed. */
  const clearRingTimeout = useCallback((): void => {
    if (ringTimerRef.current !== null) {
      clearTimeout(ringTimerRef.current);
      ringTimerRef.current = null;
    }
  }, []);

  /**
   * The video track this endpoint wants on the wire: the shared screen while a share runs, the
   * camera while it is on, and nothing otherwise.
   *
   * One accessor for one question, because three callers ask it — the quality plane shaping a rung,
   * the screen-share toggle, and the camera toggle — and three answers to "which track" would be a
   * bug waiting for the poll that lands while either toggle is being pressed. The answer itself is
   * {@link videoTrackToSend}'s, so a share, the ladder and the camera cannot disagree about which
   * track is the one being sent. A screen wins over a camera that is on, and a share still works
   * with the camera off: the camera's wish is only consulted when there is no screen to prefer.
   */
  const wantedVideoTrack = useCallback(
    (): MediaStreamTrack | null =>
      videoTrackToSend(
        screenStreamRef.current,
        cameraOnRef.current ? localStreamRef.current : null,
      ),
    [],
  );

  /**
   * Turns this side's camera on or off, for every link the ladder still carries video to.
   *
   * The track is disabled rather than stopped, which is the group plane's own choice for the same
   * toggle: the camera stays warm so turning it back on is instant and cannot fail on a device that
   * another application has since taken, and the peer sees nothing either way because the sender
   * stops carrying the track at all — a disabled track that is still attached would cost the
   * encoder to send black frames.
   */
  const toggleCamera = useCallback((): void => {
    const track = localStreamRef.current?.getVideoTracks()[0] ?? null;
    if (track === null) {
      // A voice call, or a device with no camera. The control is not drawn for either, so this is
      // only reachable from a stale screen; doing nothing is the honest answer.
      return;
    }
    const next = !cameraOnRef.current;
    cameraOnRef.current = next;
    setCameraOn(next);
    track.enabled = next;
    const sender = videoSenderRef.current;
    if (sender !== null) {
      // Shaped now rather than at the next poll, so the button does what it says on the press: the
      // same function the ladder uses, so the camera cannot come back on a link the ladder grounded.
      videoAttachedRef.current = shapeVideoSender(
        sender,
        wantedVideoTrack(),
        qualityRef.current,
        lastMeasureRef.current?.stats.sentKbps ?? 0,
        videoAttachedRef.current,
      );
    }
  }, [wantedVideoTrack]);

  /**
   * Re-reads the platform's device lists and folds them into state.
   *
   * The lists are read after the call has media, never before: a device's `label` is empty until a
   * capture permission is granted, so a menu built from an ungranted list is a menu of blanks. What
   * the platform reports is the whole answer — a deployment with one camera offers no switch, and a
   * browser with no output selection offers no speaker control (see {@link canSelectOutput}) — which
   * is why an empty list is kept rather than filled in.
   *
   * A failure here is not a failed call: a browser that refuses to enumerate leaves both lists empty
   * and the call exactly as it was, without the two controls that need them.
   */
  const refreshDevices = useCallback(async (): Promise<void> => {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      const list = cameraDevicesOf(devices);
      camerasRef.current = list;
      setCameras(list);
      setOutputs(canSelectOutput() ? outputDevicesOf(devices) : []);
    } catch {
      camerasRef.current = [];
      setCameras([]);
      setOutputs([]);
    }
  }, []);

  /**
   * Sets the audio output the call plays through, for this call and the ones after it.
   *
   * Section 180 asks for speaker, earpiece, Bluetooth, and wired headset with movement between them
   * while the call runs, and this is the whole of that on the web: the choice is state the overlay
   * applies to the media element, so it takes effect on the next frame rather than on the next call.
   * Null means the platform's default, which is what a browser that has just lost the chosen headset
   * falls back to.
   */
  const setOutputDevice = useCallback((deviceId: string | null): void => {
    setOutputId(deviceId);
  }, []);

  /**
   * Switches the call's camera to the platform's next one.
   *
   * The track is replaced rather than the stream re-acquired: the microphone must not blink, and
   * re-running `getUserMedia` for the audio half would interrupt it on every switch. The new track
   * is acquired first and only then handed over, so a switch that fails — the other camera is busy
   * in another application — leaves the call on the camera it already had rather than on none.
   *
   * The replacement goes through the same ladder shaping as everything else, so a camera switched
   * on a link the ladder has grounded is acquired and held, not sent.
   */
  const switchCamera = useCallback(async (): Promise<void> => {
    const call = activeRef.current;
    const stream = localStreamRef.current;
    const nextId = switchCameraId(camerasRef.current, cameraIdRef.current);
    if (
      call === null ||
      call.mediaKind !== CallMediaKind.Video ||
      stream === null ||
      nextId === null
    ) {
      // No call, a voice call, or a device with nothing to switch to. The control is drawn only
      // where the list had two entries and the call has a video m-line, so this is a stale screen
      // doing nothing — and on a voice call it also refuses to open a camera the call never asked
      // for and has no sender to carry.
      return;
    }
    let acquired: MediaStream;
    try {
      acquired = await navigator.mediaDevices.getUserMedia({ video: callVideoConstraints(nextId) });
    } catch {
      // The camera could not be opened — taken by another application, or unplugged since the list
      // was read. The call keeps the camera it has, which is the honest outcome.
      return;
    }
    const track = acquired.getVideoTracks()[0];
    if (track === undefined) {
      return;
    }
    const previous = stream.getVideoTracks()[0] ?? null;
    if (previous !== null) {
      stream.removeTrack(previous);
      previous.stop();
    }
    stream.addTrack(track);
    // The camera the platform actually opened, which is not necessarily the one asked for: the
    // constraint is an ideal, so the wish is corrected to the fact before the next switch reads it.
    cameraIdRef.current = track.getSettings().deviceId ?? nextId;
    // A camera acquired after the user turned video off stays off: the wish is about video, not
    // about which device provides it.
    track.enabled = cameraOnRef.current;
    setLocalStream(stream);
    const sender = videoSenderRef.current;
    if (sender !== null) {
      videoAttachedRef.current = shapeVideoSender(
        sender,
        wantedVideoTrack(),
        qualityRef.current,
        lastMeasureRef.current?.stats.sentKbps ?? 0,
        videoAttachedRef.current,
      );
    }
  }, [wantedVideoTrack]);

  /**
   * Puts a rung into effect: the tier the screen shows, the Degraded state, and the shaping of this
   * endpoint's own video.
   *
   * Everything that can move the rung goes through here — the ladder, and the two controls that cap
   * it — so a ceiling is applied to what the call does and shows rather than to the ladder itself,
   * and a control changed mid-call cannot act on a rung the ladder has already left. The measurement
   * is passed in rather than read back, because the caller is the one holding a fresh one; a control
   * that changes between samples re-applies the last numbers the ladder produced.
   */
  const applyRung = useCallback(
    (rung: LinkQuality, measuredKbps: number): void => {
      const effective = cappedQuality(rung, qualityCeilingRef.current, lowBandwidthRef.current);
      const isVideo = activeRef.current?.mediaKind === CallMediaKind.Video;
      setQuality(effective);
      setDegraded(degradedAt(effective, isVideo));
      const sender = videoSenderRef.current;
      if (sender !== null) {
        videoAttachedRef.current = shapeVideoSender(
          sender,
          wantedVideoTrack(),
          effective,
          measuredKbps,
          videoAttachedRef.current,
        );
      }
    },
    [wantedVideoTrack],
  );

  /**
   * Samples the call's link once: one reading, one rung, and — when the rung moves — one shaping of
   * this endpoint's own video and one report to the server.
   *
   * Only a move is acted on and reported. A steady link produces a reading every two seconds and
   * nothing else, which is what keeps CALL_STATS a Droppable frame an occasional call sends rather
   * than a stream of them down a link that is already the thing under suspicion. The final numbers
   * are not lost to that rule: the last measurement is kept and reported when the call ends.
   */
  const pollQuality = useCallback(async (): Promise<void> => {
    const pc = pcRef.current;
    const call = activeRef.current;
    if (pc === null || call === null) {
      return;
    }
    const counters = await readLinkCounters(pc).catch(() => null);
    if (counters === null) {
      // A connection that reports nothing usable yet is not a link in trouble; it is a link with
      // no opinion, and guessing a rung from it would degrade a call on no evidence at all.
      return;
    }
    const previous = qualityCountersRef.current;
    qualityCountersRef.current = counters;
    const now = Date.now();
    const step = advanceMeasurement(
      qualityRef.current,
      previous,
      counters,
      qualityChangedAtRef.current,
      now,
    );
    if (step === null) {
      return;
    }
    lastMeasureRef.current = { stats: step.stats, counters };
    if (!step.changed) {
      return;
    }
    // The ladder's own memory stays uncapped: it describes what the link can carry, and a user's
    // ceiling is a wish layered on top of that. Keeping the two apart is what lets a lifted ceiling
    // return the call to the rung its link is really on, rather than making it climb back through
    // rungs nobody asked for.
    qualityRef.current = step.quality;
    qualityChangedAtRef.current = now;
    applyRung(step.quality, step.stats.sentKbps);
    clientRef.current?.calls
      .reportStats(call.callId, qualityReport(step.stats, counters))
      .catch(() => {
        // CALL_STATS is Droppable: a lost report costs nothing, and the next move reports again.
      });
    // Only the rung is applied here; the shaping that reads the wanted camera moved into applyRung,
    // which is where that dependency now lives.
  }, [applyRung]);

  /**
   * Pins the call to a tier, or returns it to automatic when given null.
   *
   * Takes effect at once rather than at the next sample: a user who has just asked for a smaller
   * call is waiting for the call to become smaller, and the numbers the ladder last measured are the
   * ones the new ceiling is applied to. The rung is not recomputed — the link has not changed, only
   * what the user is willing to spend on it.
   */
  const setQualityCeiling = useCallback(
    (ceiling: LinkQuality | null): void => {
      qualityCeilingRef.current = ceiling;
      setQualityCeilingState(ceiling);
      applyRung(qualityRef.current, lastMeasureRef.current?.stats.sentKbps ?? 0);
    },
    [applyRung],
  );

  /**
   * Turns the low-bandwidth mode on or off, on a call of either kind.
   *
   * A video call gives up the ladder's top rungs and a voice call gives up audio bitrate, because a
   * voice call has no video for the ladder to act on — that is the whole of what the mode can mean
   * there, and a mode that did nothing on the call the user is actually on would be a lie.
   */
  const setLowBandwidth = useCallback(
    (on: boolean): void => {
      lowBandwidthRef.current = on;
      setLowBandwidthState(on);
      const audio = audioSenderRef.current;
      if (audio !== null) {
        shapeAudioSender(audio, on);
      }
      applyRung(qualityRef.current, lastMeasureRef.current?.stats.sentKbps ?? 0);
    },
    [applyRung],
  );

  /** Arms the quality loop for a call that just connected. Idempotent, so a re-connect is harmless. */
  const startQualityLoop = useCallback((): void => {
    qualityCountersRef.current = null;
    qualityRef.current = 'full';
    qualityChangedAtRef.current = Date.now();
    // A connect that follows a reconnect replaces the link the last rung was measured on, so the
    // screen stops claiming a tier — and a paused camera — until the new link has been sampled. A
    // rung that describes a connection that no longer exists is worse than no rung at all.
    lastMeasureRef.current = null;
    setQuality(null);
    setDegraded(false);
    if (qualityTimerRef.current !== null) {
      return;
    }
    qualityTimerRef.current = window.setInterval(() => {
      void pollQuality();
    }, QUALITY_POLL_MS);
  }, [pollQuality]);

  /** Disarms the quality loop and forgets what it measured. */
  const stopQualityLoop = useCallback((): void => {
    if (qualityTimerRef.current !== null) {
      window.clearInterval(qualityTimerRef.current);
      qualityTimerRef.current = null;
    }
    qualityCountersRef.current = null;
    lastMeasureRef.current = null;
    qualityRef.current = 'full';
    qualityChangedAtRef.current = 0;
  }, []);

  /**
   * Remembers this call's senders, so the ladder can shape the video one once the call connects and
   * the low-bandwidth mode can cap the audio one.
   *
   * The video sender is found here and nowhere else, because here is the only moment it is findable:
   * a sender that has been grounded by the bottom rung reports no track, so a later search for "the
   * video sender" would come back empty on exactly the call that needs shaping most. A voice call
   * has no video track and so adopts no video sender, which is the honest state — the ladder still
   * runs and the indicator still reports, there is simply no camera for it to act on.
   *
   * The audio sender is adopted for both kinds of call: every call has one, and it is the only
   * thing a low-bandwidth voice call can give up.
   */
  const adoptSenders = useCallback((pc: RTCPeerConnection, stream: MediaStream): void => {
    const sender =
      stream.getVideoTracks().length > 0
        ? (pc.getSenders().find((candidate) => candidate.track?.kind === 'video') ?? null)
        : null;
    videoSenderRef.current = sender;
    videoAttachedRef.current = sender !== null;
    const audio = pc.getSenders().find((candidate) => candidate.track?.kind === 'audio') ?? null;
    audioSenderRef.current = audio;
    // A mode that is already on applies to the sender the moment it exists: the call may have been
    // set to low bandwidth while this connection was still being built.
    if (audio !== null && lowBandwidthRef.current) {
      shapeAudioSender(audio, true);
    }
    // A camera that was acquired for this call starts on: the track getUserMedia returned is live,
    // and a call that opened on a black self-view would look broken rather than private.
    cameraOnRef.current = sender !== null;
    setCameraOn(sender !== null);
  }, []);

  /**
   * Ends the screen capture and forgets it. The sender is left alone, because the callers that have
   * one to hand back do it themselves and the one that does not is tearing the connection down.
   */
  const endScreenCapture = useCallback((): void => {
    const stream = screenStreamRef.current;
    screenStreamRef.current = null;
    if (stream === null) {
      return;
    }
    for (const track of stream.getTracks()) {
      // The handler is detached before the stop, so the browser's own "Stop sharing" path and this
      // one cannot re-enter each other: `stop()` fires `onended`, and a second stop would be a
      // second state update for a share that is already over.
      track.onended = null;
      track.stop();
    }
    setScreenStream(null);
  }, []);

  /**
   * Stops a screen share: the capture ends, the indicator goes out, and the camera takes the sender
   * back if the rung still has video in it and the camera is on.
   *
   * Three callers and identical in all of them — the toggle, the browser's own "Stop sharing"
   * control, and the teardown that ends the call — because a share that outlives the call it
   * belonged to is a capture still running with nothing on screen saying so.
   */
  const stopScreenShare = useCallback((): void => {
    if (screenStreamRef.current === null) {
      return;
    }
    endScreenCapture();
    setSharingScreen(false);
    const sender = videoSenderRef.current;
    // Handing the sender back is the rung's call, exactly as taking it was, and the camera's too:
    // at the bottom rung this endpoint sends no video, and a share *ending* must not be the thing
    // that puts a camera on a link the ladder grounded or switches on a camera the user turned off.
    // Both are read through the one accessor so this cannot drift from what the ladder sends. The
    // next poll shapes it either way.
    const next = wantedVideoTrack();
    if (sender !== null && qualityRef.current !== 'video-off') {
      void sender.replaceTrack(next).catch(() => {});
      videoAttachedRef.current = next !== null;
    }
  }, [endScreenCapture, wantedVideoTrack]);

  /**
   * Starts a screen share on a connected video call, or stops the one running.
   *
   * The browser's own picker is the whole of the three scopes section 180 asks for — screen,
   * window, tab — because the source cannot be forced: a menu this client drew would promise a
   * choice the browser is free to ignore. Nothing here renegotiates, and nothing here seals: a
   * video call already has a video m-line, so a share is a `replaceTrack` over media that is
   * already negotiated and already encrypted end to end, which is what section 180 means by the
   * share following the call's encryption model.
   */
  const toggleScreenShare = useCallback(async (): Promise<void> => {
    if (screenStreamRef.current !== null) {
      stopScreenShare();
      return;
    }
    const call = activeRef.current;
    if (
      call === null ||
      call.mediaKind !== CallMediaKind.Video ||
      call.state !== CallState.Connected
    ) {
      return;
    }
    let captured: MediaStream;
    try {
      captured = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
    } catch {
      // The picker was dismissed or refused, which is a choice rather than a failure: the call is
      // untouched and a message would name an error the user just decided to make.
      return;
    }
    const track = captured.getVideoTracks()[0] ?? null;
    // The picker holds the tab for as long as the user takes to choose, and the call can end under
    // it. A capture that outlives its call is the one leak this feature must not ship, so it is
    // stopped here rather than kept for a call that no longer exists.
    if (track === null || activeRef.current === null || screenStreamRef.current !== null) {
      for (const held of captured.getTracks()) {
        held.stop();
      }
      return;
    }
    screenStreamRef.current = captured;
    setScreenStream(captured);
    setSharingScreen(true);
    // The browser's own "Stop sharing" button ends the track rather than telling the page, so this
    // is the only way the interface learns the share is over — without it the indicator would keep
    // claiming a share while nothing was being sent.
    track.onended = (): void => {
      stopScreenShare();
    };
    const sender = videoSenderRef.current;
    // At the bottom rung the ladder has grounded this link and the screen stays captured but
    // unsent; the poll that climbs attaches it, because the track it wants is read from here.
    if (sender !== null && qualityRef.current !== 'video-off') {
      await sender.replaceTrack(track).catch(() => {});
      videoAttachedRef.current = true;
    }
  }, [stopScreenShare]);

  /** Stops every resource a call held: timers, candidates, the peer connection, the local media. */
  const teardownMedia = useCallback((): void => {
    stopQualityLoop();
    clearRingTimeout();
    if (iceTimerRef.current !== null) {
      clearTimeout(iceTimerRef.current);
      iceTimerRef.current = null;
    }
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    iceBatchRef.current = [];
    heldIceRef.current = [];
    remoteDescriptionSetRef.current = false;
    // An offer belongs to the call it was made on: the next call's answer must not be read as this
    // one's, and a restart left flagged here would keep the next call from ever offering its own.
    localOfferPendingRef.current = false;
    peerDeviceRef.current = null;
    setupStartRef.current = null;
    endSentRef.current = false;

    const pc = pcRef.current;
    pcRef.current = null;
    if (pc !== null) {
      pc.onicecandidate = null;
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.close();
    }

    localStreamRef.current?.getTracks().forEach((track) => track.stop());
    localStreamRef.current = null;
    setLocalStream(null);
    setRemoteStream(null);
    mutedRef.current = false;
    setMuted(false);
    // Both media wishes belong to the call that just ended; the next call adopts its own camera.
    cameraOnRef.current = false;
    setCameraOn(false);
    // The chosen camera and output belong to the call too: a device the user picked for one call is
    // not a standing preference, and carrying it into the next call would open a camera nobody
    // asked for and route audio to a headset that may have been unplugged since.
    cameraIdRef.current = null;
    setOutputId(null);
    setCameras([]);
    setOutputs([]);
    camerasRef.current = [];
    setDegraded(false);
    setQuality(null);
    // The capture is stopped here rather than handed back to a sender: the sender belongs to a
    // connection that is being closed, so the hand-back would be work for a dead object, while the
    // capture is a live window on the user's desktop that ends only when something stops it.
    endScreenCapture();
    setSharingScreen(false);
    // The sender belonged to the connection that was just closed, and the flag to the track that
    // was just stopped; keeping either would have the next call's ladder shape a dead object.
    videoSenderRef.current = null;
    videoAttachedRef.current = false;
    audioSenderRef.current = null;
    // The two quality controls are per call as well. A ceiling pinned for one call is not a standing
    // preference — the next call starts on the best rung its link can carry and re-measures from
    // there — and a low-bandwidth mode carried over would silently cap a call nobody asked to cap.
    qualityCeilingRef.current = null;
    lowBandwidthRef.current = false;
    setQualityCeilingState(null);
    setLowBandwidthState(false);
  }, [clearRingTimeout, endScreenCapture, stopQualityLoop]);

  /**
   * Ends the tracked call locally with a reason, keeping `startedAt` for the duration line.
   *
   * A `Network` reason also reaches the server as a best-effort `CALL_END` (once per call —
   * `endSentRef` keeps a hang-up and a closing tab from paying for the same exit twice), because
   * the peer otherwise waits out its whole reconnect window for a call this side already gave
   * up on.
   */
  const finishCall = useCallback(
    (reason: CallEndReason | undefined): void => {
      const call = activeRef.current;
      if (call === null || call.state === CallState.Ended) {
        return;
      }
      if (reason === CallEndReason.Network && !endSentRef.current) {
        endSentRef.current = true;
        const callId = call.callId;
        clientRef.current?.calls.end(callId, CallEndReason.Network).catch(() => {});
      }
      // The last measurement goes out here rather than only on a rung change, so a call that ran
      // the whole way on a good link still contributes its RTT, loss and jitter to the numbers
      // section 180 makes capacity decisions from. Reported before teardown forgets it.
      const last = lastMeasureRef.current;
      if (last !== null) {
        clientRef.current?.calls
          .reportStats(call.callId, qualityReport(last.stats, last.counters))
          .catch(() => {
            // Droppable, and the call is ending anyway: a lost final report costs nothing.
          });
      }
      teardownMedia();
      forgetCallKey(call.callId);
      const ended: ActiveCall = { ...call, state: CallState.Ended };
      if (reason !== undefined) {
        ended.endReason = reason;
      }
      setActive(ended);
      setEndedAt(Date.now());
    },
    [forgetCallKey, setActive, teardownMedia],
  );

  /**
   * Arms the local mirror of the invite's expiry: when it fires with the call still ringing, the
   * call ends here as {@link CallEndReason.NoAnswer} and a cancel tells the server — a callee
   * whose devices are offline is exactly who never answers, and the caller's screen must not
   * ring a call the invite no longer backs.
   */
  const armRingTimeout = useCallback(
    (callId: Id, expiresAt: number): void => {
      clearRingTimeout();
      ringTimerRef.current = window.setTimeout(
        () => {
          ringTimerRef.current = null;
          const call = activeRef.current;
          if (call === null || call.callId !== callId || call.state !== CallState.Ringing) {
            // Answered, canceled, or already ended since the timer was armed: the mirror has no
            // job left, and firing now would end a call that moved on without it.
            return;
          }
          finishCall(CallEndReason.NoAnswer);
          clientRef.current?.calls.cancel(callId).catch(() => {
            // The server sweeps its own expiry; this cancel is the prompt exit, not the only one.
          });
        },
        ringTimeoutMs(expiresAt, Date.now()),
      );
    },
    [clearRingTimeout, finishCall],
  );

  /** Marks the call connected, zeroing the duration timer exactly once and reporting setup time. */
  const markConnected = useCallback((): void => {
    const call = activeRef.current;
    if (call === null || call.state === CallState.Connected) {
      return;
    }
    const now = Date.now();
    setActive({ ...call, state: CallState.Connected, startedAt: call.startedAt ?? now });
    const setupStart = setupStartRef.current;
    if (setupStart !== null) {
      setupStartRef.current = null;
      clientRef.current?.calls.reportStats(call.callId, { setupMs: now - setupStart }).catch(() => {
        // CALL_STATS is Droppable: a lost report costs nothing.
      });
    }
    startQualityLoop();
  }, [setActive, startQualityLoop]);

  // --- ICE, both directions ---

  /** Sends the gathered candidate batch if it can be addressed and sealed; otherwise it stays queued. */
  const flushIce = useCallback((): void => {
    const call = activeRef.current;
    const target = peerDeviceRef.current;
    const batch = iceBatchRef.current;
    const key = call === null ? undefined : callKeysRef.current.get(call.callId);
    if (call === null || target === null || key === undefined || batch.length === 0) {
      return;
    }
    iceBatchRef.current = [];
    clientRef.current?.calls
      .sendIce(call.callId, target, sealCallSignal(encodeIceBatch(batch), key, call.callId))
      .catch(() => {
        // A lost batch is recovered by the next one (or the reconnect path); never fatal.
      });
  }, []);

  /** Applies the candidates the peer sent before this side's remote description existed. */
  const drainHeldIce = useCallback((): void => {
    const pc = pcRef.current;
    if (pc === null || !remoteDescriptionSetRef.current) {
      return;
    }
    const held = heldIceRef.current;
    heldIceRef.current = [];
    for (const candidate of held) {
      pc.addIceCandidate(candidate).catch(() => {
        // A candidate the connection no longer wants is normal near the end of gathering.
      });
    }
  }, []);

  /** Batches one gathered candidate, lingering briefly so a trickle leaves as few frames as it can. */
  const handleIceCandidate = useCallback(
    (event: RTCPeerConnectionIceEvent): void => {
      if (event.candidate === null) {
        // Gathering finished: whatever is batched is all there will be.
        flushIce();
        return;
      }
      iceBatchRef.current.push(event.candidate.toJSON());
      if (iceTimerRef.current === null) {
        iceTimerRef.current = window.setTimeout(() => {
          iceTimerRef.current = null;
          flushIce();
        }, ICE_LINGER_MS);
      }
    },
    [flushIce],
  );

  // --- the peer connection ---

  /**
   * Attempts an ICE restart: a fresh offer on the call's existing peer connection, sealed and
   * relayed as `CALL_RENEGOTIATE`.
   *
   * Section 180 asks for this inside the reconnect window, and it is what makes the window a
   * recovery attempt rather than only a grace period: a blip that moved the two devices' addresses —
   * a phone that changed networks, a NAT that rebound its mapping — leaves every old candidate pair
   * dead for good, so waiting cannot bring the call back and only a fresh set of candidates can. The
   * media itself needs no renegotiation: the m-lines are the ones already agreed, and the restart
   * changes the transport under them.
   *
   * Only the caller offers. An ICE restart is bidirectional the moment either side does it, so one
   * offerer is enough; two would be glare, and the rollback that resolves glare is a state this
   * build has no reason to enter. The caller is also the side that holds a target device id for the
   * whole call, where the callee's is the invite's.
   *
   * One attempt per outstanding offer, tracked by {@link localOfferPendingRef}: a second offer
   * before the first is answered would be glare with this side's own restart. A later blip can try
   * again, because the flag clears when the answer arrives or when this attempt fails to leave.
   */
  const restartIce = useCallback((): void => {
    const call = activeRef.current;
    const pc = pcRef.current;
    const target = peerDeviceRef.current;
    const key = call === null ? undefined : callKeysRef.current.get(call.callId);
    if (call === null || pc === null || target === null || key === undefined || !call.isCaller) {
      return;
    }
    if (localOfferPendingRef.current) {
      return;
    }
    localOfferPendingRef.current = true;
    void (async (): Promise<void> => {
      try {
        // `iceRestart` is the whole point: a plain offer would re-send the same candidates the dead
        // path was built from, which is a renegotiation that cannot help.
        const offer = await pc.createOffer({ iceRestart: true });
        await pc.setLocalDescription(offer);
        await clientRef.current?.calls.renegotiate(
          call.callId,
          target,
          sealCallSignal(
            encodeSdpDescription({ type: 'offer', sdp: offer.sdp ?? '' }),
            key,
            call.callId,
          ),
        );
      } catch {
        // The attempt did not leave this device — a connection that closed under it, a signaling
        // write that failed. That is a failed restart, not a failed call: the window is still open,
        // so the offer flag is cleared and a later blip inside it may try again.
        localOfferPendingRef.current = false;
      }
    })();
  }, []);

  /**
   * Builds this call's peer connection over the given ICE servers: TURN relays the server
   * configured for the call, plus the public STUN fallback (see {@link iceServersForCall}).
   */
  const createPeer = useCallback(
    (iceServers: RTCIceServer[]): RTCPeerConnection => {
      const pc = new RTCPeerConnection({ iceServers });
      pc.onicecandidate = handleIceCandidate;
      pc.ontrack = (event: RTCTrackEvent): void => {
        const stream = event.streams[0];
        if (stream !== undefined) {
          setRemoteStream(stream);
        }
      };
      pc.onconnectionstatechange = (): void => {
        const call = activeRef.current;
        if (call === null || call.state === CallState.Ended) {
          return;
        }
        if (pc.connectionState === 'connected') {
          if (reconnectTimerRef.current !== null) {
            clearTimeout(reconnectTimerRef.current);
            reconnectTimerRef.current = null;
          }
          // Media is back, so whatever offer was outstanding is moot: an answer that arrives after
          // this would be applied to a connection that no longer needs it, and the flag would keep
          // the next restart from being attempted.
          localOfferPendingRef.current = false;
          markConnected();
        } else if (pc.connectionState === 'disconnected' || pc.connectionState === 'failed') {
          // Section 180: a blip is not an end. Show Reconnecting and open the window; media back
          // cancels it, the deadline ends the call as a network failure — and inside the window the
          // caller offers a restart, which is the part that can actually bring the media back.
          //
          // `failed` gets the same treatment rather than an immediate end, because a restart is
          // exactly the remedy the specification names for it: `failed` is ICE's verdict on the
          // *current* candidate pairs, and a fresh set is the one thing that can still help. Ending
          // the call on the verdict, without offering the restart, would hang up on a call the
          // section says is recoverable.
          if (call.state !== CallState.Reconnecting) {
            setActive({ ...call, state: CallState.Reconnecting });
          }
          if (reconnectTimerRef.current === null) {
            reconnectTimerRef.current = window.setTimeout(() => {
              reconnectTimerRef.current = null;
              finishCall(CallEndReason.Network);
            }, RECONNECT_WINDOW_MS);
          }
          restartIce();
        }
      };
      pcRef.current = pc;
      return pc;
    },
    [finishCall, handleIceCandidate, markConnected, restartIce, setActive],
  );

  /**
   * Acquires the mic (and camera, for a video call) the call needs.
   *
   * The constraints are this client's own rather than the browser's defaults (see
   * {@link callAudioConstraints}): section 180 names echo cancellation, noise suppression, and
   * automatic gain control, and naming them makes the call's audio processing a decision that can
   * be read here instead of whatever the current browser release happened to choose.
   */
  const acquireMedia = async (mediaKind: CallMediaKind): Promise<MediaStream> =>
    navigator.mediaDevices.getUserMedia({
      audio: callAudioConstraints(),
      video: mediaKind === CallMediaKind.Video ? callVideoConstraints(cameraIdRef.current) : false,
    });

  // The answer acquires through the fallback below; placement does not — a user who
  // pressed "video call" asked for video, and silently handing them a voice call would
  // be the interface lying about what it did.
  /**
   * The media an *answer* needs, fallen back to what the device actually has.
   *
   * A video invite answered on a device with a microphone and no camera is still a call worth
   * taking: the audio carries the conversation and the caller simply sees no video. Declining
   * it as `Busy` would tell the caller a lie about a device that could have talked. Only the
   * camera is fungible — if the microphone itself is missing or refused, the audio-only retry
   * fails too and the original failure stands, because with no mic there is no call to answer.
   */
  const acquireAnswerMedia = useCallback(
    async (mediaKind: CallMediaKind): Promise<MediaStream> =>
      answerMediaWithFallback(mediaKind, acquireMedia),
    [],
  );

  /**
   * Adopts the media a call just acquired, and reads back what the platform actually granted.
   *
   * Both flows go through here because the same two facts need recording on each: which camera the
   * browser opened (the constraint is an ideal, so the wish must be corrected to the fact before the
   * first switch reads it) and what devices now exist — a list read only after capture is the first
   * one whose labels are not blank.
   */
  const adoptLocalStream = useCallback(
    (stream: MediaStream): void => {
      localStreamRef.current = stream;
      setLocalStream(stream);
      const track = stream.getVideoTracks()[0];
      if (track !== undefined) {
        cameraIdRef.current = track.getSettings().deviceId ?? null;
      }
      void refreshDevices();
    },
    [refreshDevices],
  );

  // --- the flows the UI calls ---

  const answerCall = useCallback(async (sealedAnswer: Uint8Array): Promise<void> => {
    const call = activeRef.current;
    const target = peerDeviceRef.current;
    const current = clientRef.current;
    if (call === null || target === null || current === null) {
      return;
    }
    await current.calls.answer(call.callId, sealedAnswer);
    await current.calls.sendSdp(call.callId, target, sealedAnswer);
  }, []);

  /**
   * Places a call: media, offer, invite — and tracks it under the id the reply echoes.
   *
   * The placement guard is a synchronous ref, not the tracked call: `activeRef` only exists once
   * the invite replies, so a second click during the microphone prompt would otherwise open a
   * second `getUserMedia` and a second peer connection — a microphone the user revoked the call
   * of, on a line nobody will ever answer. `startingRef` closes that window from the first
   * synchronous step to the last.
   */
  const startCall = useCallback(
    async (conversationId: Id, calleeId: Id, mediaKind: CallMediaKind): Promise<void> => {
      const current = clientRef.current;
      const me = accountIdRef.current;
      if (current === null || me === null) {
        return;
      }
      if (startingRef.current || callInProgress() || incomingRef.current !== null) {
        return;
      }
      startingRef.current = true;
      // Minted before the try so the catch can forget the key it adopted: a
      // placement that dies mid-flight must not leave its key in the session
      // map, and the id is the map's handle for it.
      const callId = newId();
      try {
        setupStartRef.current = Date.now();
        setCallError(null);
        // The call id is minted here rather than inside the invite so the TURN fetch can name
        // the call it belongs to: relays and credentials are for this call, and the peer
        // connection is built over them before it produces the offer the invite will carry.
        const callKey = generateCallKey();
        await current.messaging.send(conversationId, {
          type: ContentType.ControlEvent,
          event: CALL_KEY_EVENT,
          data: encodeCallKeyEvent(callId, callKey),
        });
        adoptCallKey(callId, callKey);
        const stream = await acquireMedia(mediaKind);
        adoptLocalStream(stream);

        const pc = createPeer(await iceServersForCall(current, callId));
        for (const track of stream.getTracks()) {
          pc.addTrack(track, stream);
        }
        adoptSenders(pc, stream);
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        // The invite's offer is outstanding until the answer names the device that took it: the
        // first SDP to arrive for this call is that answer, and this flag is how the handler below
        // knows to read it as one.
        localOfferPendingRef.current = true;
        const result = await current.calls.invite(
          conversationId,
          calleeId,
          mediaKind,
          sealCallSignal(
            encodeSdpDescription({ type: 'offer', sdp: offer.sdp ?? '' }),
            callKey,
            callId,
          ),
          callId,
        );

        if (result.status !== INVITE_RINGING) {
          // Never rang: the callee's own settings (or the invite's expiry) answered first.
          teardownMedia();
          forgetCallKey(callId);
          setActive({
            callId: result.callId,
            conversationId,
            callerId: me,
            calleeId,
            mediaKind,
            state: CallState.Ended,
            endReason: inviteEndReason(result.status),
            // The wire's own refusal vocabulary rides alongside the reason: a blocked refusal
            // must read differently from a declined one, and the reason enum cannot say which.
            inviteStatus: result.status,
            isCaller: true,
          });
          setEndedAt(Date.now());
          return;
        }
        setActive({
          callId: result.callId,
          conversationId,
          callerId: me,
          calleeId,
          mediaKind,
          state: CallState.Ringing,
          isCaller: true,
        });
        // A previous call's ended screen may still be up; its timestamps belong to that call.
        setEndedAt(null);
        armRingTimeout(result.callId, result.expiresAt);
      } catch (cause) {
        // Nothing was invited (permissions, no device) or the invite never landed: no call exists
        // to show, so state the failure as a fact instead of a dead button.
        teardownMedia();
        forgetCallKey(callId);
        setCallError(placementErrorMessage(cause));
      } finally {
        startingRef.current = false;
      }
    },
    [
      adoptCallKey,
      adoptLocalStream,
      adoptSenders,
      armRingTimeout,
      callInProgress,
      createPeer,
      forgetCallKey,
      setActive,
      teardownMedia,
    ],
  );

  /**
   * Answers the ringing call: media, the peer's offer applied, our answer sealed and relayed.
   *
   * A video invite answered without a camera falls back to audio rather than declining — see
   * {@link answerMediaWithFallback}; the caller keeps the conversation and simply sees no video.
   */
  const acceptCall = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    const incoming = incomingRef.current;
    const me = accountIdRef.current;
    if (current === null || incoming === null || me === null || callInProgress()) {
      return;
    }
    const mediaKind = callMediaKindOf(incoming.mediaKind);
    setupStartRef.current = Date.now();
    setIncoming(null);
    setEndedAt(null);
    setActive({
      callId: incoming.callId,
      conversationId: incoming.conversationId,
      callerId: incoming.callerId,
      calleeId: me,
      mediaKind,
      state: CallState.Connecting,
      isCaller: false,
    });
    // The invite already named the calling device, so this side's relays have a target at once.
    peerDeviceRef.current = incoming.callerDevice;
    // Whether the answer itself reached the server. The catch path's honesty
    // depends on it: before the answer lands, "cannot answer" genuinely is a
    // busy fact — this device never picked up, so the ring is free to die as
    // Busy; after it lands, the call is *connecting* and a media failure is a
    // failure, and reporting it as a busy (let alone a human decline) would
    // tell the caller a person refused what a device merely failed to do.
    let answerLanded = false;
    try {
      // The call's key arrives through the message layer, sent before the invite; on the rare
      // crossing it is waited for here rather than failed over (see {@link CALL_KEY_WAIT_MS}).
      const callKey = await waitForCallKey(incoming.callId, CALL_KEY_WAIT_MS);
      if (callKey === null) {
        throw new Error('the call key has not arrived');
      }
      const stream = await acquireAnswerMedia(mediaKind);
      adoptLocalStream(stream);

      const pc = createPeer(await iceServersForCall(current, incoming.callId));
      for (const track of stream.getTracks()) {
        pc.addTrack(track, stream);
      }
      adoptSenders(pc, stream);
      const offer = decodeSdpDescription(
        openCallSignal(incoming.sealedOffer, callKey, incoming.callId),
      );
      await pc.setRemoteDescription(offer);
      remoteDescriptionSetRef.current = true;
      drainHeldIce();

      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      await answerCall(
        sealCallSignal(
          encodeSdpDescription({ type: 'answer', sdp: answer.sdp ?? '' }),
          callKey,
          incoming.callId,
        ),
      );
      answerLanded = true;
      flushIce();
    } catch {
      // Could not answer (permissions, malformed or unopenable offer, a key that never arrived):
      // give the caller their "no" and show the failure here — a ring that can never be picked up
      // is worse than a decline.
      if (!answerLanded) {
        // The answer never reached the server, so as far as it knows this
        // device is still ringing — a decline is what retires the ring, and
        // Busy is the honest reason: occupied or unable, not unwilling.
        void current.calls.decline(incoming.callId, CallDeclineReason.Busy).catch(() => {});
      } else {
        // The answer landed, so the server already moved the call to
        // Connecting and the caller's screen left the ring behind — a decline
        // now would end an answered call as refused. End it as what it is.
        void current.calls.end(incoming.callId, CallEndReason.Failed).catch(() => {});
      }
      finishCall(CallEndReason.Failed);
    }
  }, [
    answerCall,
    acquireAnswerMedia,
    adoptLocalStream,
    adoptSenders,
    callInProgress,
    createPeer,
    drainHeldIce,
    finishCall,
    flushIce,
    setActive,
    setIncoming,
    waitForCallKey,
  ]);

  const declineCall = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    const incoming = incomingRef.current;
    setIncoming(null);
    if (current === null || incoming === null) {
      return;
    }
    await current.calls.decline(incoming.callId).catch(() => {
      // The invite expires on its own; a failed decline changes nothing on our side.
    });
  }, [setIncoming]);

  const cancelCall = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    const call = activeRef.current;
    if (current === null || call === null || call.state === CallState.Ended) {
      return;
    }
    finishCall(CallEndReason.ByCaller);
    await current.calls.cancel(call.callId).catch(() => {});
  }, [finishCall]);

  const endCall = useCallback(
    async (reason: CallEndReason): Promise<void> => {
      const current = clientRef.current;
      const call = activeRef.current;
      if (current === null || call === null || call.state === CallState.Ended) {
        return;
      }
      // Marked before the local teardown so a `Network` reason inside finishCall knows this end
      // is already on its way and does not pay for the same exit twice.
      endSentRef.current = true;
      finishCall(reason);
      await current.calls.end(call.callId, reason).catch(() => {});
    },
    [finishCall],
  );

  const toggleMute = useCallback((): void => {
    const next = !mutedRef.current;
    mutedRef.current = next;
    setMuted(next);
    for (const track of localStreamRef.current?.getAudioTracks() ?? []) {
      track.enabled = !next;
    }
  }, []);

  const dismissCall = useCallback((): void => {
    const call = activeRef.current;
    if (call !== null && call.state !== CallState.Ended) {
      return;
    }
    setActive(null);
    setEndedAt(null);
    setCallError(null);
  }, [setActive]);

  // --- the four SDK streams, registered once per session ---

  /**
   * A new invite: ring us, answer Busy for a different call if this device is occupied, and
   * ignore the redeliveries at-least-once delivery guarantees will bring.
   *
   * The decision is `incomingInviteDisposition`'s, pinned by its own tests: the redelivery of
   * the ring already showing (or of the call already answered or over) must be ignored —
   * declining it would hang up the very call the user is being rung for.
   */
  const handleIncoming = useCallback(
    (event: CallInviteEvent): void => {
      const disposition = incomingInviteDisposition(
        event,
        {
          ringingCallId: incomingRef.current?.callId ?? null,
          activeCallId: activeRef.current?.callId ?? null,
          busy: callInProgress(),
        },
        Date.now(),
      );
      if (disposition === 'ring') {
        setIncoming(event);
        return;
      }
      if (disposition === 'decline-busy') {
        clientRef.current?.calls.decline(event.callId, CallDeclineReason.Busy).catch(() => {});
      }
      // 'ignore': expired in flight, or a redelivery of a ring (or a call) this device already
      // has — expected under at-least-once delivery, and never news.
    },
    [callInProgress, setIncoming],
  );

  /**
   * The server's authoritative state transitions: for the tracked call as before, plus the one
   * event that can name a call this device never tracked.
   */
  const handleStateEvent = useCallback(
    (event: CallStateEvent): void => {
      const state = callStateOf(event.state);
      if (state === undefined) {
        // A state a newer server added: not ours to guess at, and not ours to end a live call over.
        return;
      }
      if (endsRingingCall(event, incomingRef.current?.callId ?? null)) {
        // The call this ring belongs to ended before anyone answered here — the caller canceled,
        // or the invite expired. Retire the ring and state the fact as a note: a screen that
        // keeps ringing a dead call teaches its user to distrust every ring after it. No
        // tracked call is created; this device was never in the call.
        forgetCallKey(event.callId);
        setIncoming(null);
        setCallError(MISSED_CALL_MESSAGE);
        return;
      }
      if (answersRingingCall(event, incomingRef.current?.callId ?? null)) {
        // Another device on this account answered. The server rings every device and publishes
        // the answer to both parties, so this one hears the call move on without it — retire the
        // ring and say where the call went, because "missed" would be a lie about a call that
        // connected. This device never tracked the call; there is nothing else to tear down.
        // The key is still forgotten: a sibling answered *this* call, so the only device that
        // may keep using the key is the answering one, and this one is not it.
        forgetCallKey(event.callId);
        setIncoming(null);
        setCallError(ANSWERED_ELSEWHERE_MESSAGE);
        return;
      }
      const call = activeRef.current;
      if (call === null || event.callId !== call.callId || call.state === CallState.Ended) {
        return;
      }
      if (state === CallState.Ended) {
        finishCall(callEndReasonOf(event.reason));
        return;
      }
      if (state !== CallState.Ringing) {
        // The call moved for real — answered, connecting, connected — so the invite's expiry no
        // longer ends anything and the local mirror retires.
        clearRingTimeout();
      }
      if (state === CallState.Connected) {
        markConnected();
        return;
      }
      if (state !== call.state) {
        setActive({ ...call, state });
      }
    },
    [clearRingTimeout, finishCall, forgetCallKey, markConnected, setActive, setIncoming],
  );

  /**
   * An SDP relay, read for what this side is waiting for.
   *
   * The frame does not name what it is: the server delivers a mid-call renegotiation to its target
   * as `CALL_SDP`, the same opcode an invite's answer arrives on (see the SDK's
   * {@link CallsDomain.onSdp}). {@link sdpDisposition} is the whole rule, kept pure so a test can
   * pin it; what is left here is the doing — apply the answer, or answer the peer's offer.
   */
  const handleSdp = useCallback(
    (sdp: CallSdp): void => {
      const call = activeRef.current;
      const pc = pcRef.current;
      if (call === null || sdp.callId !== call.callId || pc === null) {
        return;
      }
      const callKey = callKeysRef.current.get(call.callId);
      if (callKey === undefined) {
        // No key, nothing openable — the key message that preceded the invite was lost. For the
        // caller that is fatal: the answer is the one frame the call cannot proceed without, and
        // pretending otherwise would leave it ringing a call it can never connect. Any other SDP
        // is dropped, the same rule an unopenable ICE batch gets.
        if (localOfferPendingRef.current) {
          finishCall(CallEndReason.Failed);
        }
        return;
      }
      let description: SdpDescription;
      try {
        description = decodeSdpDescription(openCallSignal(sdp.sealedSdp, callKey, sdp.callId));
      } catch {
        // Unopenable or malformed bytes are not this side's answer: one bad frame ends nothing
        // while the connection's own signaling carries the call.
        return;
      }
      const disposition = sdpDisposition(description, localOfferPendingRef.current);
      if (disposition === SdpDisposition.Ignore) {
        return;
      }

      if (disposition === SdpDisposition.Answer) {
        localOfferPendingRef.current = false;
        pc.setRemoteDescription(description)
          .then(() => {
            remoteDescriptionSetRef.current = true;
            peerDeviceRef.current = sdp.fromDevice;
            flushIce();
            drainHeldIce();
            if (call.state === CallState.Ringing) {
              // The answer landed, so the invite's expiry has no call left to end: the ring's
              // local mirror retires with the state it was arming against.
              clearRingTimeout();
              setActive({ ...call, state: CallState.Connecting });
            }
          })
          .catch(() => {
            finishCall(CallEndReason.Failed);
          });
        return;
      }

      // The peer's renegotiation: an ICE restart, whose fresh candidates arrive as ICE relays
      // around this frame. The media needs nothing new — the m-lines are the ones already agreed —
      // so the offer is applied and answered, sealed and sent back the way it came.
      const fromDevice = sdp.fromDevice;
      void (async (): Promise<void> => {
        try {
          await pc.setRemoteDescription(description);
          remoteDescriptionSetRef.current = true;
          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);
          await clientRef.current?.calls.sendSdp(
            call.callId,
            fromDevice,
            sealCallSignal(
              encodeSdpDescription({ type: 'answer', sdp: answer.sdp ?? '' }),
              callKey,
              call.callId,
            ),
          );
        } catch {
          // The restart could not be answered. The call is left as it was rather than ended here:
          // the transport is down on this side, the peer's own window is running, and the reconnect
          // deadline is what decides — ending it from here would hang up on the very attempt the
          // section asks for.
        }
      })();
    },
    [clearRingTimeout, finishCall, flushIce, drainHeldIce, setActive],
  );

  /** A batch of the peer's candidates, applied now or held until the remote description exists. */
  const handleIceRelay = useCallback((ice: CallIce): void => {
    const call = activeRef.current;
    const pc = pcRef.current;
    if (call === null || ice.callId !== call.callId || pc === null) {
      return;
    }
    let candidates: RTCIceCandidateInit[];
    try {
      const callKey = callKeysRef.current.get(call.callId);
      if (callKey === undefined) {
        // Same rule as an unopenable batch: dropped, not fatal, while the connection's own
        // gathering carries the call.
        return;
      }
      candidates = decodeIceBatch(openCallSignal(ice.sealedCandidates, callKey, ice.callId));
    } catch {
      // One malformed batch is dropped, not fatal: the next batch or the connection's own
      // gathering carries the call.
      return;
    }
    if (remoteDescriptionSetRef.current) {
      for (const candidate of candidates) {
        pc.addIceCandidate(candidate).catch(() => {});
      }
    } else {
      heldIceRef.current.push(...candidates);
    }
  }, []);

  useEffect(() => {
    if (!client) {
      // The session dropped mid-call: there is no signaling left to end it with, so end it here.
      if (callInProgress()) {
        finishCall(CallEndReason.Network);
      }
      return;
    }
    // The call key rides the message layer, not the call streams, so its subscription lives
    // beside them: a control event naming a call-key event adopts the key, waking an accept that
    // is waiting on it. Anything else — every ordinary message — is not this manager's business.
    const offKeyEvents = client.messaging.onMessage((message) => {
      const content = message.content;
      if (content.type !== ContentType.ControlEvent || content.event !== CALL_KEY_EVENT) {
        return;
      }
      const decoded = content.data === undefined ? null : decodeCallKeyEvent(content.data);
      if (decoded !== null) {
        adoptCallKey(decoded.callId, decoded.key);
      }
    });
    const offs = [
      offKeyEvents,
      client.calls.onIncomingCall(handleIncoming),
      client.calls.onCallState(handleStateEvent),
      client.calls.onSdp(handleSdp),
      client.calls.onIce(handleIceRelay),
    ];
    return () => {
      for (const off of offs) {
        off();
      }
    };
  }, [
    adoptCallKey,
    client,
    callInProgress,
    finishCall,
    handleIncoming,
    handleStateEvent,
    handleSdp,
    handleIceRelay,
  ]);

  // Closing the tab mid-call must tell the server and the peer: without this, the other side
  // sits out its whole reconnect window before learning the call is over. `sendBeacon` cannot
  // carry a gateway frame, so the RPC is simply fired without awaiting it — whether the frame
  // beats the socket's death is the browser's race, and losing it costs only the peer's wait.
  // The reason is this side's hang-up, not `Network`: the user chose to leave, and the peer's
  // screen should say the call ended, not that a connection was lost.
  useEffect(() => {
    const onPageUnload = (): void => {
      const call = activeRef.current;
      const current = clientRef.current;
      if (
        current === null ||
        call === null ||
        call.state === CallState.Ended ||
        endSentRef.current
      ) {
        return;
      }
      endSentRef.current = true;
      void current.calls
        .end(call.callId, call.isCaller ? CallEndReason.ByCaller : CallEndReason.ByCallee)
        .catch(() => {});
    };
    window.addEventListener('beforeunload', onPageUnload);
    return () => window.removeEventListener('beforeunload', onPageUnload);
  }, []);

  /**
   * Keeps the device lists current while a call is up. A headset plugged in mid-call, or a camera
   * another application releases, is a device the platform reports through `devicechange` — and
   * section 180's movement between outputs is exactly the case where the list has to move under the
   * menu without the call being restarted to see it.
   */
  useEffect(() => {
    if (activeCall === null || activeCall.state === CallState.Ended) {
      return;
    }
    const devices = navigator.mediaDevices;
    if (devices === undefined || typeof devices.addEventListener !== 'function') {
      return;
    }
    const onDeviceChange = (): void => {
      void refreshDevices();
    };
    devices.addEventListener('devicechange', onDeviceChange);
    return () => devices.removeEventListener('devicechange', onDeviceChange);
  }, [activeCall, refreshDevices]);

  // Unmounting the shell must not leave a microphone on.
  useEffect(
    () => (): void => {
      teardownMedia();
      setActive(null);
      setIncoming(null);
      setEndedAt(null);
    },
    [teardownMedia, setActive, setIncoming],
  );

  const value: CallManagerValue = {
    activeCall,
    incomingCall,
    muted,
    cameraOn,
    degraded,
    quality,
    qualityCeiling,
    lowBandwidth,
    sharingScreen,
    screenStream,
    outputs,
    cameras,
    outputId,
    localStream,
    remoteStream,
    endedAt,
    callError,
    startCall,
    acceptCall,
    answerCall,
    declineCall,
    cancelCall,
    endCall,
    toggleMute,
    toggleCamera,
    switchCamera,
    setOutputDevice,
    setQualityCeiling,
    setLowBandwidth,
    toggleScreenShare,
    dismissCall,
  };

  return <CallManagerContext.Provider value={value}>{children}</CallManagerContext.Provider>;
}

/** Access to the call manager. Throws if used outside {@link CallManagerProvider}. */
export function useCall(): CallManagerValue {
  const value = useContext(CallManagerContext);
  if (value === null) {
    throw new Error('useCall must be used within a CallManagerProvider');
  }
  return value;
}

/** What went wrong before any call existed, said as a fact — never a payload or a stack trace. */
function placementErrorMessage(cause: unknown): string {
  if (
    cause instanceof DOMException &&
    (cause.name === 'NotAllowedError' || cause.name === 'NotFoundError')
  ) {
    return 'Microphone or camera unavailable. Check permissions and try again.';
  }
  return 'Could not start the call.';
}
