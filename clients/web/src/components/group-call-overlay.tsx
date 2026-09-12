'use client';

/**
 * The group-call screen: the roster, the count, and the words for every way the call can stop.
 *
 * The component splits the way the call surface does. {@link GroupCallOverlay} is the thin
 * context-connected half — it reads the group-call manager, resolves participant names, and keeps
 * a one-second clock ticking while seated. {@link GroupCallScreen} is the pure half every state
 * renders through, so each screen state is pinnable by a test without a socket or a context.
 *
 * # The states, and why each names itself
 *
 * Section 180's rule for 1:1 calls is the rule here: a call screen that goes silent without a
 * sentence is a screen its user closes and distrusts. So *Joining…* while the seat is requested,
 * the participant count once seated, and — when the call stops — one of four distinct notes: a
 * leave, the call's retirement, the seat continuing on this account's other device, or a lost
 * session. None of them collapses into "call ended", because they are different facts about what
 * the user should do next.
 *
 * # No media, said honestly
 *
 * This build carries the roster, not the media plane. The screen renders avatars and names and
 * says so in one dim line — a "voice call" that silently played nothing would be the interface
 * lying about what it did.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { formatCallDuration, mediaKindLabel } from '@/lib/migo/call-signal.js';
import type { ActiveGroupCall } from '@/lib/migo/group-call-manager.js';
import { groupCallNoteLabel } from '@/lib/migo/group-roster.js';
import { useGroupCall } from '@/lib/migo/group-call-manager.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { CallErrorCard } from './call-overlay.js';

/** Everything the pure roster screen needs; every callback is the manager's, already bound. */
export interface GroupCallScreenProps {
  /** The tracked group call, including one that just ended. */
  call: ActiveGroupCall;
  /** Display names keyed by participant account id; an unknown account falls back to a generic. */
  names: ReadonlyMap<Id, string>;
  /** This session's account, for the roster's "you" mark. */
  meId: Id | null;
  /** The clock the duration reads, passed in so the pure half has no timer of its own. */
  nowMs: number;
  onLeave: () => void;
  onDismiss: () => void;
}

/**
 * The group-call screen, pure. The roster is shown in join order for every phase — a joining
 * screen simply has an empty list — and the actions follow the phase: a hang-up while the seat is
 * live, a way back to the app once a note has replaced it.
 */
export function GroupCallScreen({
  call,
  names,
  meId,
  nowMs,
  onLeave,
  onDismiss,
}: GroupCallScreenProps): ReactNode {
  const live = call.note === null;
  const seated = live && call.phase === 'seated';
  const rosterEmpty = live && call.phase === 'joining';

  return (
    <div className="call-overlay" role="dialog" aria-modal="true" aria-label="Group call">
      <div className="call-identity">
        <div className="call-name">Group {mediaKindLabel(call.mediaKind)}</div>
        <div className="call-status" aria-live="polite">
          {call.note !== null
            ? groupCallNoteLabel(call.note)
            : rosterEmpty
              ? 'Joining…'
              : `${call.participantCount} in this call`}
        </div>
        {seated && call.joinedAt !== null ? (
          <div className="call-timer" role="timer">
            {formatCallDuration(nowMs - call.joinedAt)}
          </div>
        ) : null}
        {seated ? (
          <div className="call-roster-note">Roster only in this build — media arrives later.</div>
        ) : null}
      </div>

      <ul className="group-call-roster" aria-label="Participants">
        {call.seats.map((seat) => {
          const name = names.get(seat.userId) ?? 'Migo member';
          return (
            <li key={seat.userId} className="group-call-seat">
              <Avatar name={name} id={seat.userId} size={32} />
              <span className="group-call-seat-name">{name}</span>
              {meId !== null && seat.userId === meId ? <span className="tag">You</span> : null}
            </li>
          );
        })}
      </ul>

      <div className="call-actions">
        {live ? (
          <button
            type="button"
            className="call-action hang-up"
            aria-label={rosterEmpty ? 'Cancel joining the call' : 'Leave the call'}
            onClick={onLeave}
          >
            ✕
          </button>
        ) : (
          <button type="button" className="btn btn-ghost" onClick={onDismiss}>
            Back to chats
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * The context-connected half: renders over the whole shell while this device holds a group-call
 * seat (or shows why it lost one), and nothing at all otherwise.
 */
export function GroupCallOverlay(): ReactNode {
  const { activeGroupCall, groupCallError, leaveGroupCall, dismissGroupCall } = useGroupCall();
  const { accountId } = useMigo();

  const ids = activeGroupCall === null ? [] : activeGroupCall.seats.map((seat) => seat.userId);
  const profiles = useProfiles(ids);
  // Names resolved once, here, so the pure half renders strings and nothing else: the same split
  // the call screen keeps between its connected and pure halves.
  const names = new Map<Id, string>();
  for (const [id, profile] of profiles) {
    names.set(id, profile.displayName ?? profile.username ?? 'Migo member');
  }

  // One tick per second while seated: the duration is the only number on screen that moves.
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  useEffect(() => {
    if (activeGroupCall === null || activeGroupCall.note !== null) {
      return;
    }
    // Re-zero on entering seated, so the first shown second is this call's, not the mount's.
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [activeGroupCall]);

  if (activeGroupCall === null) {
    if (groupCallError !== null) {
      return (
        <CallErrorCard
          message={groupCallError}
          onDismiss={dismissGroupCall}
          label="Group call failed"
        />
      );
    }
    return null;
  }

  return (
    <GroupCallScreen
      call={activeGroupCall}
      names={names}
      meId={accountId}
      nowMs={nowMs}
      onLeave={() => void leaveGroupCall()}
      onDismiss={dismissGroupCall}
    />
  );
}
