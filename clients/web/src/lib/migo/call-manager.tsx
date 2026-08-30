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
 * A transport blip must not end a call (section 180): the peer connection going `disconnected`
 * shows *Reconnecting* and starts a window; media coming back cancels it; the window expiring ends
 * the call with `Network` — and, since the server and the peer still think the call is live, fires
 * a best-effort `CALL_END` so the other side is spared the whole window. This build does not yet
 * attempt the ICE restart inside the window — that is `CALL_RENEGOTIATE`'s job and a future task —
 * so the window is a grace period, not a recovery attempt.
 *
 * # The ring's lifecycle
 *
 * Three facts keep a ring honest. Invites are Critical frames, delivered at least once, so a
 * redelivered invite names a call this device already knows — it is ignored, never declined, or
 * the decline would hang up the very ring it re-announces. An unanswered invite ends itself at
 * `expiresAt`; the caller arms a local mirror of that deadline so its "Calling…" screen never
 * outlives the invite even if the server's `Ended` event is late or lost. And an `Ended` for the
 * call still ringing inbound retires the ring with a missed-call note — the caller gave up, and a
 * screen that keeps ringing a dead call is teaching its user to distrust every ring after it.
 *
 * # The placeholder seal
 *
 * Every SDP and ICE blob this manager sends is wrapped by {@link sealCallSignal} — placeholder
 * key material behind a real envelope shape, the same posture image attachments use. The server
 * relays bytes it cannot read either way; swapping in real per-device sealing touches the seal
 * module alone.
 *
 * Media content is never logged here — not the SDP, not the candidates, not the streams; failures
 * are recorded as facts ("could not start the call"), not payloads.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { CallDeclineReason, CallEndReason, CallMediaKind, CallState, newId } from '@migo/sdk';
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
  INVITE_RINGING,
  callEndReasonOf,
  callMediaKindOf,
  callStateOf,
  decodeIceBatch,
  decodeSdpDescription,
  endsRingingCall,
  encodeIceBatch,
  encodeSdpDescription,
  incomingInviteDisposition,
  inviteEndReason,
  openCallSignal,
  ringTimeoutMs,
  sealCallSignal,
} from './call-signal.js';
import { useMigo } from './use-migo.js';

/** How long gathered ICE candidates linger before one relay carries them (section 165: batch, briefly). */
const ICE_LINGER_MS = 250;
/** How long a disconnected transport gets before the call ends as a network failure. */
const RECONNECT_WINDOW_MS = 30_000;
/**
 * What the missed-call note says when an inbound ring retires because the call ended before it
 * was answered. Exported so the overlay can label its card with the same fact the manager states.
 */
export const MISSED_CALL_MESSAGE = 'Missed call';

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
   * Whether a connected call's quality has fallen far enough to pause video. Always false in this
   * build — the statistics feed that would flip it is future work — but the state it drives is the
   * sixth screen state section 180 requires, so the plumbing is here for it to land in.
   */
  degraded: boolean;
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
  /** Dismisses the ended screen (or a placement error), leaving no call tracked. */
  dismissCall: () => void;
}

const CallManagerContext = createContext<CallManagerValue | null>(null);

export function CallManagerProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId } = useMigo();

  const [activeCall, setActiveCall] = useState<ActiveCall | null>(null);
  const [incomingCall, setIncomingCall] = useState<CallInviteEvent | null>(null);
  const [muted, setMuted] = useState(false);
  const [degraded, setDegraded] = useState(false);
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
  /** The peer device relays are addressed to; null until the answer names it (or the invite did). */
  const peerDeviceRef = useRef<Id | null>(null);
  /** Candidates gathered but not yet relayed, waiting for the batch linger or a target device. */
  const iceBatchRef = useRef<RTCIceCandidateInit[]>([]);
  const iceTimerRef = useRef<number | null>(null);
  /** Candidates the peer relayed before this side's remote description was set, applied after it is. */
  const heldIceRef = useRef<RTCIceCandidateInit[]>([]);
  const remoteDescriptionSetRef = useRef(false);
  const reconnectTimerRef = useRef<number | null>(null);
  /** The caller's local mirror of the invite's expiry, while its call still rings unanswered. */
  const ringTimerRef = useRef<number | null>(null);
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

  // --- tracked-state writers: ref first (handlers read it synchronously), then React state ---

  const setActive = useCallback((call: ActiveCall | null): void => {
    activeRef.current = call;
    setActiveCall(call);
  }, []);

  const setIncoming = useCallback((event: CallInviteEvent | null): void => {
    incomingRef.current = event;
    setIncomingCall(event);
  }, []);

  /** Whether a call occupies this device — an ended call still on screen does not block a new one. */
  const callInProgress = useCallback(
    (): boolean => activeRef.current !== null && activeRef.current.state !== CallState.Ended,
    [],
  );

  // --- teardown ---

  /** Disarms the caller's local expiry mirror; safe to call when nothing is armed. */
  const clearRingTimeout = useCallback((): void => {
    if (ringTimerRef.current !== null) {
      clearTimeout(ringTimerRef.current);
      ringTimerRef.current = null;
    }
  }, []);

  /** Stops every resource a call held: timers, candidates, the peer connection, the local media. */
  const teardownMedia = useCallback((): void => {
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
    setDegraded(false);
  }, [clearRingTimeout]);

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
      teardownMedia();
      const ended: ActiveCall = { ...call, state: CallState.Ended };
      if (reason !== undefined) {
        ended.endReason = reason;
      }
      setActive(ended);
      setEndedAt(Date.now());
    },
    [setActive, teardownMedia],
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
  }, [setActive]);

  // --- ICE, both directions ---

  /** Sends the gathered candidate batch if it can be addressed; otherwise it stays queued. */
  const flushIce = useCallback((): void => {
    const call = activeRef.current;
    const target = peerDeviceRef.current;
    const batch = iceBatchRef.current;
    if (call === null || target === null || batch.length === 0) {
      return;
    }
    iceBatchRef.current = [];
    clientRef.current?.calls
      .sendIce(call.callId, target, sealCallSignal(encodeIceBatch(batch)))
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
          markConnected();
        } else if (pc.connectionState === 'disconnected') {
          // Section 180: a blip is not an end. Show Reconnecting and open the window; media back
          // cancels it, the deadline ends the call as a network failure.
          if (call.state !== CallState.Reconnecting) {
            setActive({ ...call, state: CallState.Reconnecting });
          }
          if (reconnectTimerRef.current === null) {
            reconnectTimerRef.current = window.setTimeout(() => {
              reconnectTimerRef.current = null;
              finishCall(CallEndReason.Network);
            }, RECONNECT_WINDOW_MS);
          }
        } else if (pc.connectionState === 'failed') {
          finishCall(CallEndReason.Network);
        }
      };
      pcRef.current = pc;
      return pc;
    },
    [finishCall, handleIceCandidate, markConnected, setActive],
  );

  /** Acquires the mic (and camera, for a video call) the call needs. */
  const acquireMedia = async (mediaKind: CallMediaKind): Promise<MediaStream> =>
    navigator.mediaDevices.getUserMedia({
      audio: true,
      video: mediaKind === CallMediaKind.Video,
    });

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
      try {
        setupStartRef.current = Date.now();
        setCallError(null);
        // The call id is minted here rather than inside the invite so the TURN fetch can name
        // the call it belongs to: relays and credentials are for this call, and the peer
        // connection is built over them before it produces the offer the invite will carry.
        const callId = newId();
        const stream = await acquireMedia(mediaKind);
        localStreamRef.current = stream;
        setLocalStream(stream);

        const pc = createPeer(await iceServersForCall(current, callId));
        for (const track of stream.getTracks()) {
          pc.addTrack(track, stream);
        }
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        const result = await current.calls.invite(
          conversationId,
          calleeId,
          mediaKind,
          sealCallSignal(encodeSdpDescription({ type: 'offer', sdp: offer.sdp ?? '' })),
          callId,
        );

        if (result.status !== INVITE_RINGING) {
          // Never rang: the callee's own settings (or the invite's expiry) answered first.
          teardownMedia();
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
        setCallError(placementErrorMessage(cause));
      } finally {
        startingRef.current = false;
      }
    },
    [armRingTimeout, callInProgress, createPeer, setActive, teardownMedia],
  );

  /** Answers the ringing call: media, the peer's offer applied, our answer sealed and relayed. */
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
    try {
      const stream = await acquireMedia(mediaKind);
      localStreamRef.current = stream;
      setLocalStream(stream);

      const pc = createPeer(await iceServersForCall(current, incoming.callId));
      for (const track of stream.getTracks()) {
        pc.addTrack(track, stream);
      }
      const offer = decodeSdpDescription(openCallSignal(incoming.sealedOffer));
      await pc.setRemoteDescription(offer);
      remoteDescriptionSetRef.current = true;
      drainHeldIce();

      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      await answerCall(
        sealCallSignal(encodeSdpDescription({ type: 'answer', sdp: answer.sdp ?? '' })),
      );
      flushIce();
    } catch {
      // Could not answer (permissions, malformed offer): give the caller their "no" and show the
      // failure here — a ring that can never be picked up is worse than a decline.
      void current.calls.decline(incoming.callId, CallDeclineReason.Busy).catch(() => {});
      finishCall(CallEndReason.Failed);
    }
  }, [
    answerCall,
    callInProgress,
    createPeer,
    drainHeldIce,
    finishCall,
    flushIce,
    setActive,
    setIncoming,
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
        setIncoming(null);
        setCallError(MISSED_CALL_MESSAGE);
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
    [clearRingTimeout, finishCall, markConnected, setActive, setIncoming],
  );

  /** An SDP relay: for a caller this is the answer naming the device everything now addresses. */
  const handleSdp = useCallback(
    (sdp: CallSdp): void => {
      const call = activeRef.current;
      const pc = pcRef.current;
      if (call === null || sdp.callId !== call.callId || pc === null) {
        return;
      }
      if (!call.isCaller || remoteDescriptionSetRef.current) {
        // A renegotiated offer mid-call is CALL_RENEGOTIATE's flow, not this build's.
        return;
      }
      let description: { type: 'offer' | 'answer' | 'pranswer' | 'rollback'; sdp: string };
      try {
        description = decodeSdpDescription(openCallSignal(sdp.sealedSdp));
      } catch {
        finishCall(CallEndReason.Failed);
        return;
      }
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
      candidates = decodeIceBatch(openCallSignal(ice.sealedCandidates));
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
    const offs = [
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
    degraded,
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
