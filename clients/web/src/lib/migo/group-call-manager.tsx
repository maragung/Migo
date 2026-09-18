'use client';

/**
 * The group-call manager: one React context that owns this device's seat in one group call, the
 * map of the calls it could still join, and the call's media plane.
 *
 * The SDK's group-call domain is pure signaling — it sends what it is handed and delivers what
 * arrives. This provider is the piece above it a roster UI needs: it joins with a sealed
 * placeholder offer (see {@link ./group-roster.ts} for why the placeholder is really sealed),
 * projects the tracked {@link ActiveGroupCall} the roster screen renders from the three SDK
 * events, keeps the calls in progress in conversations this device is not seated in — the fact a
 * header button turns into "join the running call" — and owns the exits — a leave, the call's
 * retirement, a seat replacement by this account's other device, a dropped session — each with its
 * own words on the screen, because a roster that vanishes without a sentence teaches its user to
 * distrust the next one.
 *
 * # The media plane
 *
 * The seat and the frame key are what the plane needs, and both exist by the time the seat is
 * accepted: the SDK mints the first seat's key on the roster snapshot, a mid-call joiner's key
 * arrives with a distributor's answer, and {@link MigoClient.callKeys} says which is which
 * (`hasKey` now, `onKeyChanged` the moment it becomes true or advances). So the manager starts
 * exactly one {@link GroupMediaPlane} per seated call — never before the key is held, because
 * nothing the plane seals could be opened otherwise — feeds it the roster projection and the
 * sealed SDP/ICE relays, and drives its quality tick. Every exit path (leave, retirement, seat
 * replacement, dropped session, unmount, tab close) tears it down through the same one function,
 * so no link and no local track outlives the seat that owned them. The join frame's sealed offer
 * stays the roster module's placeholder for the reason {@link ./group-media.ts} states: the
 * joiner holds no frame key at the moment of joining, so the real descriptions ride per-peer
 * offers on the relay, where both ends already hold the key.
 *
 * One call at a time. A device holds one seat in one call in this build; the join guard is a
 * synchronous check of the tracked call, the same discipline the 1:1 manager keeps for its
 * placement guard.
 *
 * # The events, and which of them are ours
 *
 * The join announcements and departure announcements ride the *conversation's* topic: every
 * member's client hears them, seated or not. Each announcement has two readers here: the roster
 * of the call this device joined (matched by call id — a join from this account's other device is
 * that device's screen), and the in-progress map of calls in conversations this device is *not*
 * seated in, which is what lets a member who has not joined see the running call and join it by
 * its id, rather than minting a second call the conversation did not ask for.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { CallMediaKind, newId } from '@migo/sdk';
import type { Id, TurnServer } from '@migo/sdk';

import { generateCallKey } from './call-signal.js';
import { GroupMediaPlane, groupIceServers } from './group-media.js';
import type { GroupMediaLink } from './group-media.js';
import {
  inProgressArrived,
  inProgressDeparted,
  inProgressFromListing,
  isCallRetired,
  namesOwnSeat,
  placeholderSealedOffer,
  seatArrived,
  seatDeparted,
  seatsFromSnapshot,
} from './group-roster.js';
import type { GroupCallNote, GroupCallSeat, InProgressGroupCall } from './group-roster.js';
import { useMigo } from './use-migo.js';

/** What a group call that could not even be joined says, as a fact. */
export const GROUP_CALL_JOIN_FAILED = 'Could not join the group call.';

/** How often the media plane's quality ladder reads its links and moves a rung. */
const QUALITY_TICK_MS = 3_000;

/**
 * The empty in-progress map, shared: the folds below always build a fresh map, so no write ever
 * reaches into this one.
 */
const NO_CALLS_IN_PROGRESS: ReadonlyMap<Id, InProgressGroupCall> = new Map();

/**
 * The tracked group call: the seat list as this device last knew it, the phase of our own seat,
 * and the media the plane is holding. Kept by the manager and rendered by the roster screen; every
 * field the screen shows is a field here, so a screen state is pinnable by a test without a
 * socket.
 */
export interface ActiveGroupCall {
  callId: Id;
  conversationId: Id;
  /** What the join carried: the label, and what the plane acquires (a camera when video). */
  mediaKind: CallMediaKind;
  /** `joining` until the join reply or roster snapshot lands; `seated` after. */
  phase: 'joining' | 'seated';
  /** Why a live call stopped being one; `null` while the seat is ours. */
  note: GroupCallNote | null;
  /** The roster, in join order. Empty while joining; frozen when a note replaces it. */
  seats: GroupCallSeat[];
  /** The call's size as the server last counted it — the number the screen states. */
  participantCount: number;
  /** When this device's seat was accepted, for the duration line. */
  joinedAt: number | null;
  /** The media links this device's plane holds, in join order; empty until media exists. */
  links: GroupMediaLink[];
  /** The plane's local stream once media exists — the self-view's source. */
  localStream: MediaStream | null;
  /** Whether this seat publishes video (it wanted to, and the product limit admitted it). */
  videoPublished: boolean;
  /** Whether this seat's microphone is muted. */
  muted: boolean;
  /** Whether this seat's camera is on; `null` when no camera was published. */
  cameraOn: boolean | null;
  /** A media failure stated as a fact (the microphone the plane could not acquire). */
  mediaError: string | null;
}

/** What the rest of the app reads and calls. */
export interface GroupCallManagerValue {
  /** The group call this device is in, including one that just ended (until dismissed). */
  activeGroupCall: ActiveGroupCall | null;
  /** Why a group call could not be joined, when nothing else is showing. */
  groupCallError: string | null;
  /**
   * The call running in a conversation this device is not seated in, when one is — the fact a
   * header's join affordance names and joins by id.
   */
  groupCallInProgress: (conversationId: Id) => InProgressGroupCall | null;
  /**
   * Joins the conversation's group call: the running call's id when the caller has one (seating
   * into it), a freshly minted id otherwise. The media kind chooses what the seat acquires — a
   * camera joins as video within the product limit.
   */
  joinGroupCall: (conversationId: Id, callId?: Id, mediaKind?: CallMediaKind) => Promise<void>;
  /** Leaves the tracked call: the screen turns to its "left" note, and the server frees the seat. */
  leaveGroupCall: () => Promise<void>;
  /** Dismisses the ended screen, leaving no call tracked. */
  dismissGroupCall: () => void;
  /**
   * Mutes or unmutes the seated call's microphone, everywhere at once. Returns the new muted
   * state, or `null` when no microphone exists to mute.
   */
  toggleGroupMute: () => boolean | null;
  /**
   * Turns the seated call's camera on or off, everywhere at once. Returns the new state, or `null`
   * when no camera was published (an audio seat, or video the product limit refused).
   */
  toggleGroupCamera: () => boolean | null;
  /**
   * The remote stream of one roster device, once its link's tracks have arrived — the audio
   * element's source. Read during a render that follows a link change, so a stream that just
   * arrived is an element the same render attaches.
   */
  groupRemoteStream: (deviceId: Id) => MediaStream | null;
}

const GroupCallManagerContext = createContext<GroupCallManagerValue | null>(null);

export function GroupCallManagerProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId, deviceId } = useMigo();

  const [activeGroupCall, setActiveGroupCall] = useState<ActiveGroupCall | null>(null);
  const [groupCallError, setGroupCallError] = useState<string | null>(null);
  // The calls running in conversations this device is not seated in, keyed by conversation. The
  // announcements keep it true and nothing else does, so it is honestly empty until the first one
  // lands — a member who opens the thread between movements sees "join" until the roster speaks.
  const [trackedCalls, setTrackedCalls] =
    useState<ReadonlyMap<Id, InProgressGroupCall>>(NO_CALLS_IN_PROGRESS);

  // The event handlers are registered once per client, so everything they read must be a ref.
  const clientRef = useRef(client);
  clientRef.current = client;
  const accountIdRef = useRef(accountId);
  accountIdRef.current = accountId;
  const deviceIdRef = useRef(deviceId);
  deviceIdRef.current = deviceId;
  const activeRef = useRef<ActiveGroupCall | null>(null);
  const trackedRef = useRef<ReadonlyMap<Id, InProgressGroupCall>>(NO_CALLS_IN_PROGRESS);

  // The media plane of the seated call, and what it needs: the join reply's TURN list, the frame
  // key's epoch as this device last saw it (a null epoch means "no key held yet"), and the quality
  // tick's timer. All ref-held because the plane is created and torn down by call-flow events, not
  // by renders — a re-render must never rebuild a live peer connection.
  const planeRef = useRef<GroupMediaPlane | null>(null);
  const turnServersRef = useRef<TurnServer[]>([]);
  const keyEpochRef = useRef<number | null>(null);
  const tickRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // --- tracked-state writers: ref first (handlers read it synchronously), then React state ---

  const setActive = useCallback((call: ActiveGroupCall | null): void => {
    activeRef.current = call;
    setActiveGroupCall(call);
  }, []);

  const setTracked = useCallback((calls: ReadonlyMap<Id, InProgressGroupCall>): void => {
    trackedRef.current = calls;
    setTrackedCalls(calls);
  }, []);

  // --- the media plane's lifecycle ---

  /**
   * Tears the media plane down, completely: links closed, local tracks stopped, tick disarmed. One
   * function for every exit path, so no link and no track outlives the seat that owned them.
   */
  const teardownMedia = useCallback((): void => {
    if (tickRef.current !== null) {
      clearInterval(tickRef.current);
      tickRef.current = null;
    }
    planeRef.current?.leave();
    planeRef.current = null;
    keyEpochRef.current = null;
  }, []);

  /**
   * Starts the seated call's media plane, if it can exist and does not yet.
   *
   * "Can exist" is three facts: the seat is accepted, the call's frame key is held (nothing the
   * plane seals could be opened before it is — the first seat's key is minted on the snapshot, a
   * joiner's arrives with a distributor's answer), and the seat is still this device's. The join
   * reply's TURN relays ride the plane's ICE servers, with the public STUN fallback behind them.
   */
  const maybeStartMedia = useCallback((): void => {
    const current = clientRef.current;
    const active = activeRef.current;
    if (
      current === null ||
      planeRef.current !== null ||
      active === null ||
      active.note !== null ||
      active.phase !== 'seated'
    ) {
      return;
    }
    if (!current.callKeys.hasKey(active.callId)) {
      // A joiner's key ask is still in flight; `onKeyChanged` is what re-runs this.
      return;
    }
    const accountId = accountIdRef.current;
    const deviceId = deviceIdRef.current;
    if (accountId === null || deviceId === null) {
      return;
    }
    const callId = active.callId;
    const plane = new GroupMediaPlane({
      callId,
      conversationId: active.conversationId,
      accountId,
      deviceId,
      mediaKind: active.mediaKind,
      iceServers: groupIceServers(turnServersRef.current),
      createPeer: (iceServers) => new RTCPeerConnection({ iceServers }),
      acquire: (kind) =>
        navigator.mediaDevices.getUserMedia({
          audio: true,
          video: kind === CallMediaKind.Video,
        }),
      // The camera on its own, for the seat that turned its own off and wants it back: a second
      // `acquire` would reopen the microphone too, which the seat is speaking into.
      acquireCamera: async () => {
        try {
          const camera = await navigator.mediaDevices.getUserMedia({ video: true });
          return camera.getVideoTracks()[0] ?? null;
        } catch {
          return null;
        }
      },
      sendSdp: (toDevice, sealed) => current.calls.sendSdp(callId, toDevice, sealed),
      sendIce: (toDevice, sealed) => current.calls.sendIce(callId, toDevice, sealed),
      seal: (frame) => current.callKeys.sealFrame(callId, frame),
      open: (sealed) => current.callKeys.openFrame(callId, sealed),
      onLinks: (links) => {
        const planeNow = planeRef.current;
        const activeNow = activeRef.current;
        if (planeNow === null || activeNow === null || activeNow.callId !== callId) {
          return;
        }
        setActive({
          ...activeNow,
          links,
          localStream: planeNow.localStream,
          videoPublished: planeNow.videoPublished,
          muted: planeNow.muted,
          cameraOn: planeNow.cameraOn,
        });
      },
      onFailure: (what) => {
        const activeNow = activeRef.current;
        if (activeNow !== null && activeNow.callId === callId) {
          setActive({ ...activeNow, mediaError: what });
        }
      },
    });
    planeRef.current = plane;
    void plane.begin(active.seats).catch(() => {
      // begin's own failures are already stated through onFailure; this is the unexpected rest.
    });
    tickRef.current = setInterval(() => {
      void planeRef.current?.tick(Date.now()).catch(() => {
        // A tick that cannot read its links is a tick the next one retries.
      });
    }, QUALITY_TICK_MS);
  }, [setActive]);

  /**
   * The frame key changed state for the seated call. A first-held key is what makes sealing
   * possible (the plane starts through {@link maybeStartMedia}); an epoch advance invalidates the
   * sealing of every negotiation still in flight, so the plane resets its unconnected links and
   * the dialer re-dials under the fresh key — connected links keep flowing, their media riding
   * DTLS-SRTP, which the frame key does not gate.
   */
  const onKeyChangedFor = useCallback(
    (callId: Id): void => {
      const current = clientRef.current;
      if (current === null) {
        return;
      }
      const epoch = current.callKeys.keyEpoch(callId);
      const previous = keyEpochRef.current;
      keyEpochRef.current = epoch;
      if (previous === null) {
        maybeStartMedia();
        return;
      }
      if (epoch !== null && epoch > previous) {
        planeRef.current?.keyEpochChanged();
      }
    },
    [maybeStartMedia],
  );

  // --- the flows the UI calls ---

  /**
   * Joins the conversation's group call.
   *
   * The call id is the caller's when the join answers a call already running — the header's
   * "join in progress" affordance hands the tracked call's id over for exactly this, and reusing
   * it is the point: client-minted ids are the protocol's idempotency key, so this join seats
   * into the running call rather than minting a second one the conversation did not ask for. A
   * fresh join mints the id here, and a retried join re-seats the same call either way. The
   * offer is the roster module's sealed placeholder: the joiner holds no frame key at the moment
   * of joining, so the real media descriptions ride per-peer offers on the relay once the key
   * arrives (see {@link ./group-media.ts}).
   *
   * The roster snapshot the screen builds from does not come back on the reply: the server
   * *publishes* it to this account's own topic, and the snapshot handler below may fire before or
   * after the reply resolves — both orders reach the same screen, because the snapshot is what
   * marks the seat accepted, not the reply. The reply does carry the call's TURN relays, which
   * the media plane's peer connections dial through.
   */
  const joinGroupCall = useCallback(
    async (conversationId: Id, callId?: Id, mediaKind = CallMediaKind.Audio): Promise<void> => {
      const current = clientRef.current;
      if (current === null || accountIdRef.current === null || deviceIdRef.current === null) {
        return;
      }
      // One seat in one call per device in this build; a second press of the button is not a
      // second call but a no-op, the same discipline the 1:1 manager keeps for its placement
      // guard. The guard reads into a local: narrowing the ref's property directly would pin it
      // as `null` for the rest of the function (the write the join flows through happens inside
      // setActive, out of the narrowing's sight), and the post-await reads below need the ref's
      // full type.
      const seated = activeRef.current;
      if (seated !== null) {
        return;
      }
      const joinedCallId = callId ?? newId();
      setActive({
        callId: joinedCallId,
        conversationId,
        mediaKind,
        phase: 'joining',
        note: null,
        seats: [],
        participantCount: 0,
        joinedAt: null,
        links: [],
        localStream: null,
        videoPublished: false,
        muted: false,
        cameraOn: null,
        mediaError: null,
      });
      // Seating in the tracked call makes the roster this conversation's call state; the
      // tracker's entry has nothing left to tell the header.
      if (trackedRef.current.get(conversationId)?.callId === joinedCallId) {
        const without = new Map(trackedRef.current);
        without.delete(conversationId);
        setTracked(without);
      }
      try {
        const joined = await current.groupCalls.join(
          conversationId,
          mediaKind,
          placeholderSealedOffer(generateCallKey(), joinedCallId),
          joinedCallId,
        );
        turnServersRef.current = joined.servers;
        // The snapshot may already have marked the seat accepted (it is published, not replied);
        // this only fills in the timestamp for a reply that won the race. Either order has the
        // key in place by now for a first seat, so the plane may start; a joiner whose key ask
        // is still in flight is started by `onKeyChanged` instead.
        const active = activeRef.current;
        if (active !== null && active.callId === joinedCallId && active.phase === 'joining') {
          setActive({ ...active, phase: 'seated', joinedAt: active.joinedAt ?? Date.now() });
        }
        onKeyChangedFor(joinedCallId);
      } catch {
        // The join never landed (membership refused, the socket gone). A snapshot that arrived
        // despite the failure means it did land — only fail a call still waiting for its seat.
        const active = activeRef.current;
        if (active !== null && active.callId === joinedCallId && active.phase === 'joining') {
          setActive(null);
          setGroupCallError(GROUP_CALL_JOIN_FAILED);
        }
      }
    },
    [setActive, setTracked, onKeyChangedFor],
  );

  /**
   * Leaves the tracked call.
   *
   * The screen turns to its "left" note at once, the media plane is torn down at once, and the
   * leave follows best-effort: the seat may already be gone (the call retired, or another device
   * replaced it), and a leave for a seat this device no longer holds is the server's NOT_FOUND,
   * not a fact the user needs.
   */
  const leaveGroupCall = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    const active = activeRef.current;
    if (current === null || active === null || active.note !== null) {
      return;
    }
    teardownMedia();
    setActive({ ...active, note: 'left', links: [] });
    await current.groupCalls.leave(active.callId).catch(() => {
      // See above — the screen is already honest.
    });
  }, [setActive, teardownMedia]);

  const dismissGroupCall = useCallback((): void => {
    const active = activeRef.current;
    if (active !== null && active.note === null) {
      // A live call is not dismissable; the leave button is its exit.
      return;
    }
    teardownMedia();
    setActive(null);
    setGroupCallError(null);
  }, [setActive, teardownMedia]);

  /** Mutes the seated call's microphone, or says there is none to mute. */
  const toggleGroupMute = useCallback((): boolean | null => {
    const plane = planeRef.current;
    if (plane === null) {
      return null;
    }
    const muted = plane.toggleMute();
    const active = activeRef.current;
    if (active !== null) {
      setActive({ ...active, muted });
    }
    return muted;
  }, [setActive]);

  /** Turns the seated call's camera on or off, or says no camera was published. */
  const toggleGroupCamera = useCallback((): boolean | null => {
    const plane = planeRef.current;
    if (plane === null) {
      return null;
    }
    const cameraOn = plane.toggleCamera();
    const active = activeRef.current;
    if (active !== null && cameraOn !== null) {
      setActive({ ...active, cameraOn });
    }
    return cameraOn;
  }, [setActive]);

  // --- the SDK streams, registered once per session ---

  /**
   * The roster snapshot: the authoritative seat list for a join this device made. Snapshots for
   * any other call id are this account's *other* devices joining — that device's screen, not
   * this one's — and are ignored. The announcements below ride the conversation's topic and reach
   * every member's client, seated or not; the same call-id match keeps the roster fold to this
   * device's call, and everything the match turns away feeds the in-progress map instead.
   */

  useEffect(() => {
    if (!client) {
      // The session dropped mid-call: there is no signaling left to leave with, so the screen
      // states what happened and stops. The seat may persist server-side until this device
      // returns; this build does not auto-re-join on resume. The plane is torn down at once —
      // its links' relay is gone with the session, and a live mic outliving its call is a fact
      // nobody asked the user for.
      const active = activeRef.current;
      teardownMedia();
      if (active !== null && active.note === null) {
        setActive({ ...active, note: 'connection', links: [] });
      }
      // The tracked calls are announcement-kept; with no signaling left they could only go
      // stale, and a stale entry is a join the server would refuse.
      setTracked(NO_CALLS_IN_PROGRESS);
      return;
    }
    const offs = [
      client.groupCalls.onRoster((roster) => {
        const active = activeRef.current;
        if (active === null || active.callId !== roster.callId) {
          return;
        }
        const seats = seatsFromSnapshot(roster);
        setActive({
          ...active,
          phase: 'seated',
          seats,
          participantCount: roster.participantCount,
          joinedAt: active.joinedAt ?? Date.now(),
        });
        onKeyChangedFor(roster.callId);
        planeRef.current?.seatsChanged(seats);
      }),
      client.groupCalls.onParticipantJoined((event) => {
        const active = activeRef.current;
        // The announcements name every member's movement, so the call-id match is what says
        // which of the two readers below an event is for: the roster when this device holds a
        // seat in that call (frozen once a note ends it), the in-progress map when it does not.
        if (active !== null && active.callId === event.callId) {
          if (active.note !== null) {
            return;
          }
          const seats = seatArrived(active.seats, event, Date.now());
          setActive({
            ...active,
            seats,
            participantCount: event.participantCount,
          });
          planeRef.current?.seatsChanged(seats);
          return;
        }
        setTracked(inProgressArrived(trackedRef.current, event));
      }),
      client.groupCalls.onParticipantLeft((event) => {
        const active = activeRef.current;
        // The same match as the arrival above, with the same split of readers.
        if (active !== null && active.callId === event.callId) {
          if (active.note !== null) {
            return;
          }
          const accountId = accountIdRef.current;
          const deviceId = deviceIdRef.current;
          if (
            accountId !== null &&
            deviceId !== null &&
            namesOwnSeat(event, { accountId, deviceId })
          ) {
            // This connection never hears its own leave (the server skips the origin session when
            // publishing), so a departure naming this exact account *and* device is the seat being
            // replaced from this account's other device — the call continues, just not here.
            teardownMedia();
            setActive({ ...active, note: 'moved', links: [] });
            return;
          }
          const retired = isCallRetired(event);
          if (retired) {
            teardownMedia();
          }
          const seats = seatDeparted(active.seats, event);
          setActive({
            ...active,
            note: retired ? 'ended' : null,
            seats,
            participantCount: event.participantCount,
            ...(retired ? { links: [] } : {}),
          });
          if (!retired) {
            planeRef.current?.seatsChanged(seats);
          }
          return;
        }
        setTracked(inProgressDeparted(trackedRef.current, event));
      }),
      // The frame key's state changes: a first-held key starts the plane, an epoch advance
      // resets the negotiations the rotation stranded.
      client.callKeys.onKeyChanged((callId) => {
        const active = activeRef.current;
        if (active !== null && active.callId === callId) {
          onKeyChangedFor(callId);
        }
      }),
      // The sealed SDP and ICE relays of every call — the 1:1 plane's frames and this plane's
      // ride the same two opcodes, and each listener keeps only what it can open.
      client.calls.onSdp((event) => {
        void planeRef.current?.onSdp(event).catch(() => {
          // A relay the plane could not process is one its key change will reset.
        });
      }),
      client.calls.onIce((event) => {
        planeRef.current?.onIce(event);
      }),
    ];
    // The calls this session cannot have heard about. The announcements below only reach a client
    // that was connected to hear them, so a member who was offline through an *entire* group call
    // would keep an empty map until the next join or departure — which, for a call that is already
    // running, never comes. One listing at session start asks the server which calls this account
    // can see, and the fold (see {@link inProgressFromListing}) adds whichever ones this session
    // never heard announced. Without it, the header's "join the running call" is missing for
    // exactly the member who just came back.
    //
    // The answer is dropped if the effect was torn down first: a listing that lands after the
    // session it was asked on is a fact about a session that no longer exists. A server that does
    // not know the opcode yet is the same shape of non-fact — the map simply stays as the event
    // stream keeps it, which is what this client did before the listing existed.
    let sessionCurrent = true;
    void client.calls
      .listCalls()
      .then((entries) => {
        if (sessionCurrent) {
          setTracked(inProgressFromListing(trackedRef.current, entries));
        }
      })
      .catch(() => {
        // Nothing to do: the announcements remain the map's source, as they were.
      });
    return () => {
      sessionCurrent = false;
      for (const off of offs) {
        off();
      }
    };
  }, [client, setActive, setTracked, teardownMedia, onKeyChangedFor]);

  // Closing the tab while seated must tell the server, the same contract the 1:1 manager keeps:
  // without it the seat lingers until the server notices the dead session. The RPC is fired
  // without awaiting it — whether the frame beats the socket's death is the browser's race. The
  // plane goes first: its links close with the page, and a mic left on by an orphaned track is
  // the fact nobody asked for.
  useEffect(() => {
    const onPageUnload = (): void => {
      const active = activeRef.current;
      const current = clientRef.current;
      if (current === null || active === null || active.note !== null) {
        return;
      }
      teardownMedia();
      void current.groupCalls.leave(active.callId).catch(() => {});
    };
    window.addEventListener('beforeunload', onPageUnload);
    return () => window.removeEventListener('beforeunload', onPageUnload);
  }, [teardownMedia]);

  // Unmounting the shell must not leave a tracked call, a live plane, or a running tick behind.
  useEffect(
    () => (): void => {
      teardownMedia();
      setActive(null);
      setTracked(NO_CALLS_IN_PROGRESS);
    },
    [setActive, setTracked, teardownMedia],
  );

  /** The call running in a conversation this device is not seated in, when one is. */
  const groupCallInProgress = useCallback(
    (conversationId: Id): InProgressGroupCall | null => trackedCalls.get(conversationId) ?? null,
    [trackedCalls],
  );

  /** The remote stream of one roster device, once its link's tracks have arrived. */
  const groupRemoteStream = useCallback(
    (deviceId: Id): MediaStream | null => planeRef.current?.remoteStreamOf(deviceId) ?? null,
    [],
  );

  const value: GroupCallManagerValue = {
    activeGroupCall,
    groupCallError,
    groupCallInProgress,
    joinGroupCall,
    leaveGroupCall,
    dismissGroupCall,
    toggleGroupMute,
    toggleGroupCamera,
    groupRemoteStream,
  };

  return (
    <GroupCallManagerContext.Provider value={value}>{children}</GroupCallManagerContext.Provider>
  );
}

/** Access to the group-call manager. Throws if used outside {@link GroupCallManagerProvider}. */
export function useGroupCall(): GroupCallManagerValue {
  const value = useContext(GroupCallManagerContext);
  if (value === null) {
    throw new Error('useGroupCall must be used within a GroupCallManagerProvider');
  }
  return value;
}
