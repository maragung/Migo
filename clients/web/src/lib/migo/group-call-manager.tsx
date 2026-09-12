'use client';

/**
 * The group-call manager: one React context that owns this device's seat in one group call.
 *
 * The SDK's group-call domain is pure signaling — it sends what it is handed and delivers what
 * arrives. This provider is the piece above it a roster UI needs: it joins with a sealed
 * placeholder offer (see {@link ./group-roster.ts} for why the placeholder is really sealed),
 * projects the tracked {@link ActiveGroupCall} the roster screen renders from the three SDK
 * events, and owns the exits — a leave, the call's retirement, a seat replacement by this
 * account's other device, a dropped session — each with its own words on the screen, because a
 * roster that vanishes without a sentence teaches its user to distrust the next one.
 *
 * # What this manager deliberately does not do
 *
 * No media. This build carries the roster, not the media plane: no `getUserMedia`, no peer
 * connections, no microphone to tear down. The join's sealed offer is the placeholder the roster
 * module seals, and the frame-key distribution between participants that a real media plane needs
 * (section 165's remaining client-side SPEC) is future work — this manager is shaped so that work
 * lands beside it, not inside it.
 *
 * One call at a time. A device holds one seat in one call in this build; the join guard is a
 * synchronous check of the tracked call, the same discipline the 1:1 manager keeps for its
 * placement guard.
 *
 * # The events, and which of them are ours
 *
 * The join announcements and departure announcements ride the *conversation's* topic: every
 * member's client hears them, seated or not. This manager tracks only the call this device joined
 * (matched by call id — a join from this account's other device is that device's screen), so an
 * announcement for any other call is ignored here. A future "call in progress" banner for
 * not-yet-seated members would subscribe to the same stream; it is not this build's shape.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { CallMediaKind, newId } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { generateCallKey } from './call-signal.js';
import {
  isCallRetired,
  namesOwnSeat,
  placeholderSealedOffer,
  seatArrived,
  seatDeparted,
  seatsFromSnapshot,
} from './group-roster.js';
import type { GroupCallNote, GroupCallSeat } from './group-roster.js';
import { useMigo } from './use-migo.js';

/** What a group call that could not even be joined says, as a fact. */
export const GROUP_CALL_JOIN_FAILED = 'Could not join the group call.';

/**
 * The tracked group call: the seat list as this device last knew it, plus the phase of our own
 * seat. Kept by the manager and rendered by the roster screen; every field the screen shows is a
 * field here, so a screen state is pinnable by a test without a socket.
 */
export interface ActiveGroupCall {
  callId: Id;
  conversationId: Id;
  /** What the join carried; the label and nothing else in this build — no media rides it yet. */
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
}

/** What the rest of the app reads and calls. */
export interface GroupCallManagerValue {
  /** The group call this device is in, including one that just ended (until dismissed). */
  activeGroupCall: ActiveGroupCall | null;
  /** Why a group call could not be joined, when nothing else is showing. */
  groupCallError: string | null;
  /** Joins the conversation's group call (or re-seats this device in one it knows the id of). */
  joinGroupCall: (conversationId: Id) => Promise<void>;
  /** Leaves the tracked call: the screen turns to its "left" note, and the server frees the seat. */
  leaveGroupCall: () => Promise<void>;
  /** Dismisses the ended screen, leaving no call tracked. */
  dismissGroupCall: () => void;
}

const GroupCallManagerContext = createContext<GroupCallManagerValue | null>(null);

export function GroupCallManagerProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId, deviceId } = useMigo();

  const [activeGroupCall, setActiveGroupCall] = useState<ActiveGroupCall | null>(null);
  const [groupCallError, setGroupCallError] = useState<string | null>(null);

  // The event handlers are registered once per client, so everything they read must be a ref.
  const clientRef = useRef(client);
  clientRef.current = client;
  const accountIdRef = useRef(accountId);
  accountIdRef.current = accountId;
  const deviceIdRef = useRef(deviceId);
  deviceIdRef.current = deviceId;
  const activeRef = useRef<ActiveGroupCall | null>(null);

  // --- tracked-state writer: ref first (handlers read it synchronously), then React state ---

  const setActive = useCallback((call: ActiveGroupCall | null): void => {
    activeRef.current = call;
    setActiveGroupCall(call);
  }, []);

  // --- the flows the UI calls ---

  /**
   * Joins the conversation's group call.
   *
   * The call id is minted here — client-minted ids are the protocol's idempotency key, so a
   * retried join re-seats the same call — and the offer is the roster module's sealed placeholder:
   * this build's honest stand-in for a media description, sealed under a per-call key exactly as
   * the real one will be (section 165's rule is about what the server sees, not about whether the
   * media plane has landed).
   *
   * The roster snapshot the screen builds from does not come back on the reply: the server
   * *publishes* it to this account's own topic, and the snapshot handler below may fire before or
   * after the reply resolves — both orders reach the same screen, because the snapshot is what
   * marks the seat accepted, not the reply.
   */
  const joinGroupCall = useCallback(
    async (conversationId: Id): Promise<void> => {
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
      const callId = newId();
      setActive({
        callId,
        conversationId,
        mediaKind: CallMediaKind.Audio,
        phase: 'joining',
        note: null,
        seats: [],
        participantCount: 0,
        joinedAt: null,
      });
      try {
        await current.groupCalls.join(
          conversationId,
          CallMediaKind.Audio,
          placeholderSealedOffer(generateCallKey(), callId),
          callId,
        );
        // The snapshot may already have marked the seat accepted (it is published, not replied);
        // this only fills in the timestamp for a reply that won the race.
        const active = activeRef.current;
        if (active !== null && active.callId === callId && active.phase === 'joining') {
          setActive({ ...active, phase: 'seated', joinedAt: active.joinedAt ?? Date.now() });
        }
      } catch {
        // The join never landed (membership refused, the socket gone). A snapshot that arrived
        // despite the failure means it did land — only fail a call still waiting for its seat.
        const active = activeRef.current;
        if (active !== null && active.callId === callId && active.phase === 'joining') {
          setActive(null);
          setGroupCallError(GROUP_CALL_JOIN_FAILED);
        }
      }
    },
    [setActive],
  );

  /**
   * Leaves the tracked call.
   *
   * The screen turns to its "left" note at once and the leave follows best-effort: the seat may
   * already be gone (the call retired, or another device replaced it), and a leave for a seat this
   * device no longer holds is the server's NOT_FOUND, not a fact the user needs.
   */
  const leaveGroupCall = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    const active = activeRef.current;
    if (current === null || active === null || active.note !== null) {
      return;
    }
    setActive({ ...active, note: 'left' });
    await current.groupCalls.leave(active.callId).catch(() => {
      // See above — the screen is already honest.
    });
  }, [setActive]);

  const dismissGroupCall = useCallback((): void => {
    const active = activeRef.current;
    if (active !== null && active.note === null) {
      // A live call is not dismissable; the leave button is its exit.
      return;
    }
    setActive(null);
    setGroupCallError(null);
  }, [setActive]);

  // --- the three SDK streams, registered once per session ---

  /**
   * The roster snapshot: the authoritative seat list for a join this device made. Snapshots for
   * any other call id are this account's *other* devices joining — that device's screen, not
   * this one's — and are ignored. The announcements below ride the conversation's topic and reach
   * every member's client; the same call-id match keeps this manager to this device's call.
   */

  useEffect(() => {
    if (!client) {
      // The session dropped mid-call: there is no signaling left to leave with, so the screen
      // states what happened and stops. The seat may persist server-side until this device
      // returns; this build does not auto-re-join on resume.
      const active = activeRef.current;
      if (active !== null && active.note === null) {
        setActive({ ...active, note: 'connection' });
      }
      return;
    }
    const offs = [
      client.groupCalls.onRoster((roster) => {
        const active = activeRef.current;
        if (active === null || active.callId !== roster.callId) {
          return;
        }
        setActive({
          ...active,
          phase: 'seated',
          seats: seatsFromSnapshot(roster),
          participantCount: roster.participantCount,
          joinedAt: active.joinedAt ?? Date.now(),
        });
      }),
      client.groupCalls.onParticipantJoined((event) => {
        const active = activeRef.current;
        if (active === null || active.callId !== event.callId || active.note !== null) {
          return;
        }
        setActive({
          ...active,
          seats: seatArrived(active.seats, event, Date.now()),
          participantCount: event.participantCount,
        });
      }),
      client.groupCalls.onParticipantLeft((event) => {
        const active = activeRef.current;
        if (active === null || active.callId !== event.callId || active.note !== null) {
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
          setActive({ ...active, note: 'moved' });
          return;
        }
        setActive({
          ...active,
          note: isCallRetired(event) ? 'ended' : null,
          seats: seatDeparted(active.seats, event),
          participantCount: event.participantCount,
        });
      }),
    ];
    return () => {
      for (const off of offs) {
        off();
      }
    };
  }, [client, setActive]);

  // Closing the tab while seated must tell the server, the same contract the 1:1 manager keeps:
  // without it the seat lingers until the server notices the dead session. The RPC is fired
  // without awaiting it — whether the frame beats the socket's death is the browser's race.
  useEffect(() => {
    const onPageUnload = (): void => {
      const active = activeRef.current;
      const current = clientRef.current;
      if (current === null || active === null || active.note !== null) {
        return;
      }
      void current.groupCalls.leave(active.callId).catch(() => {});
    };
    window.addEventListener('beforeunload', onPageUnload);
    return () => window.removeEventListener('beforeunload', onPageUnload);
  }, []);

  // Unmounting the shell must not leave a tracked call behind.
  useEffect(
    () => (): void => {
      setActive(null);
    },
    [setActive],
  );

  const value: GroupCallManagerValue = {
    activeGroupCall,
    groupCallError,
    joinGroupCall,
    leaveGroupCall,
    dismissGroupCall,
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
