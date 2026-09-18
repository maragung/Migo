'use client';

/**
 * The group-call screen: the roster, the count, the media, and the words for every way the call
 * can stop.
 *
 * The component splits the way the call surface does. {@link GroupCallOverlay} is the thin
 * context-connected half — it reads the group-call manager, resolves participant names, hands the
 * remote streams to the audio elements, and keeps a one-second clock ticking while seated.
 * {@link GroupCallScreen} is the pure half every state renders through, so each screen state is
 * pinnable by a test without a socket or a context.
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
 * The same rule holds per seat, and {@link groupSeatState} is where it lives: a link still
 * negotiating says *Connecting…*, a link the quality ladder has moved says *Degraded*, a video
 * seat the product limit refused video says *Audio only*, a muted seat says *Muted*. A seat with
 * no word is a seat whose media is simply flowing — the quiet case does not announce itself,
 * because a screen that labels everything labels nothing.
 */

import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { CallMediaKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { formatCallDuration, mediaKindLabel } from '@/lib/migo/call-signal.js';
import type { ActiveGroupCall } from '@/lib/migo/group-call-manager.js';
import { groupCallNoteLabel } from '@/lib/migo/group-roster.js';
import type { GroupCallSeat } from '@/lib/migo/group-roster.js';
import { useGroupCall } from '@/lib/migo/group-call-manager.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { CallErrorCard } from './call-overlay.js';

/**
 * The one word (or short phrase) a seat's state earns on the roster, or `null` when the seat's
 * media is simply flowing.
 *
 * The own seat and the remote seats read differently on purpose. This device's own seat states
 * what its user controls and what was refused: *Muted* while the mic is, *Camera off* while the
 * camera is, *Audio only* when the seat asked for video and the product limit did not admit it —
 * the ninth video stream is refused as a stream, never as a participant. A remote seat states
 * what its link is doing: *Connecting…* until the link connects, *Degraded* once the quality
 * ladder has moved off the top rung, *Connection lost* when the link failed. A remote seat whose
 * link has not been dialed yet (two seats that both wait for the other to dial cannot happen —
 * the join order decides — but a roster announcement can arrive before its offer does) is still
 * *Connecting…*, which is the honest word for "known to the roster, not yet to the plane".
 */
export function groupSeatState(
  seat: GroupCallSeat,
  call: ActiveGroupCall,
  meId: Id | null,
): string | null {
  if (meId !== null && seat.userId === meId) {
    return ownSeatState(call);
  }
  const link = call.links.find((entry) => entry.deviceId === seat.deviceId);
  if (link === undefined) {
    // The roster knows the seat; the plane's link to it does not exist yet. The only seat that
    // can honestly be linkless forever is this device's own.
    return 'Connecting…';
  }
  if (link.phase === 'failed') {
    return 'Connection lost';
  }
  if (link.phase === 'connecting') {
    return 'Connecting…';
  }
  if (link.phase === 'closed') {
    return 'Left';
  }
  if (link.quality !== 'full') {
    return 'Degraded';
  }
  return null;
}

/**
 * The own seat's state words — what this device's user controls and what was refused. Separate
 * from {@link groupSeatState} because the own seat has no link to itself to read; its words come
 * from the call's own media state.
 */
export function ownSeatState(call: ActiveGroupCall): string | null {
  // The share is stated first because it is the fact that changes what the other seats see: a
  // muted microphone and a share are both true at once, and the roster has one line to say them in.
  if (call.sharingScreen) {
    return 'Sharing screen';
  }
  if (call.muted) {
    return 'Muted';
  }
  if (call.mediaKind === CallMediaKind.Video && !call.videoPublished) {
    // The seat asked for video; the product limit (or a camera that would not start) refused it.
    return 'Audio only';
  }
  if (call.cameraOn === false) {
    return 'Camera off';
  }
  return null;
}

/**
 * Plays one remote seat's stream: an `<audio>` element whose source is assigned in an effect,
 * because `srcObject` is a property, not an attribute — static markup can only carry the element;
 * the browser half wires the stream once mounted.
 */
export function RemoteAudio({ stream }: { stream: MediaStream }): ReactNode {
  const ref = useRef<HTMLAudioElement | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (element === null) {
      return;
    }
    element.srcObject = stream;
    void element.play().catch(() => {
      // Autoplay refused until the user interacts; the element is still attached, so the first
      // interaction starts it. The overlay's own controls count as that interaction.
    });
  }, [stream]);
  return <audio ref={ref} autoPlay />;
}

/** Shows this seat's own camera to itself: muted by construction, so it never echoes. */
export function SelfVideo({ stream }: { stream: MediaStream }): ReactNode {
  const ref = useRef<HTMLVideoElement | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (element !== null) {
      element.srcObject = stream;
    }
  }, [stream]);
  return <video ref={ref} autoPlay muted playsInline className="group-call-self-video" />;
}

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
  /** The remote seats' streams, keyed by device id — the audio elements' sources. */
  streams: ReadonlyMap<Id, MediaStream>;
  onLeave: () => void;
  onDismiss: () => void;
  /** Mutes or unmutes this seat's microphone; the manager's, already bound. */
  onToggleMute: () => boolean | null;
  /** Turns this seat's camera on or off; the manager's, already bound. */
  onToggleCamera: () => boolean | null;
  /** Starts or stops this seat's screen share; the manager's, already bound. */
  onToggleScreenShare: () => Promise<boolean>;
}

/**
 * The group-call screen, pure. The roster is shown in join order for every phase — a joining
 * screen simply has an empty list — and the actions follow the phase: a hang-up while the seat is
 * live, a way back to the app once a note has replaced it, and — while media exists — the mic and
 * camera controls section 180 asks for.
 */
export function GroupCallScreen({
  call,
  names,
  meId,
  nowMs,
  streams,
  onLeave,
  onDismiss,
  onToggleMute,
  onToggleCamera,
  onToggleScreenShare,
}: GroupCallScreenProps): ReactNode {
  const live = call.note === null;
  const seated = live && call.phase === 'seated';
  const rosterEmpty = live && call.phase === 'joining';
  const cameraToggleable = call.videoPublished;

  return (
    <div className="call-overlay" role="dialog" aria-modal="true" aria-label="Group call">
      <div className="call-identity">
        <div className="call-name">Group {mediaKindLabel(call.mediaKind)}</div>
        <div className="call-status" aria-live="polite">
          {call.note !== null
            ? groupCallNoteLabel(call.note)
            : rosterEmpty
              ? 'Joining…'
              : call.mediaError !== null
                ? call.mediaError
                : `${call.participantCount} in this call`}
        </div>
        {seated && call.joinedAt !== null ? (
          <div className="call-timer" role="timer">
            {formatCallDuration(nowMs - call.joinedAt)}
          </div>
        ) : null}
      </div>

      {seated && call.localStream !== null && call.videoPublished && call.cameraOn !== false ? (
        <SelfVideo stream={call.localStream} />
      ) : null}

      <ul className="group-call-roster" aria-label="Participants">
        {call.seats.map((seat) => {
          const name = names.get(seat.userId) ?? 'Migo member';
          const isMe = meId !== null && seat.userId === meId;
          const state = groupSeatState(seat, call, meId);
          const stream = streams.get(seat.deviceId);
          return (
            <li key={seat.userId} className="group-call-seat">
              <Avatar name={name} id={seat.userId} size={32} />
              <span className="group-call-seat-name">{name}</span>
              {state !== null ? <span className="group-call-seat-state">{state}</span> : null}
              {isMe ? <span className="tag">You</span> : null}
              {stream !== undefined && stream.getAudioTracks().length > 0 ? (
                <RemoteAudio stream={stream} />
              ) : null}
            </li>
          );
        })}
      </ul>

      <div className="call-actions">
        {live ? (
          <>
            {seated ? (
              <button
                type="button"
                className={`icon-btn call-action mute${call.muted ? ' muted' : ''}`}
                aria-label={call.muted ? 'Unmute microphone' : 'Mute microphone'}
                title={call.muted ? 'Unmute microphone' : 'Mute microphone'}
                onClick={onToggleMute}
              >
                {call.muted ? '🔇' : '🎤'}
              </button>
            ) : null}
            {seated && cameraToggleable ? (
              <button
                type="button"
                className={`icon-btn call-action mute${call.cameraOn === false ? ' muted' : ''}`}
                aria-label={call.cameraOn === false ? 'Turn camera on' : 'Turn camera off'}
                title={call.cameraOn === false ? 'Turn camera on' : 'Turn camera off'}
                onClick={onToggleCamera}
              >
                {call.cameraOn === false ? '📷' : '🎥'}
              </button>
            ) : null}
            {seated && cameraToggleable ? (
              <button
                type="button"
                className={`icon-btn call-action mute${call.sharingScreen ? ' muted' : ''}`}
                aria-label={call.sharingScreen ? 'Stop sharing your screen' : 'Share your screen'}
                title={call.sharingScreen ? 'Stop sharing your screen' : 'Share your screen'}
                onClick={onToggleScreenShare}
              >
                {call.sharingScreen ? '🛑' : '🖥️'}
              </button>
            ) : null}
            <button
              type="button"
              className="call-action hang-up"
              aria-label={rosterEmpty ? 'Cancel joining the call' : 'Leave the call'}
              onClick={onLeave}
            >
              ✕
            </button>
          </>
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
  const {
    activeGroupCall,
    groupCallError,
    leaveGroupCall,
    dismissGroupCall,
    toggleGroupMute,
    toggleGroupCamera,
    toggleGroupScreenShare,
    groupRemoteStream,
  } = useGroupCall();
  const { accountId } = useMigo();

  const ids = activeGroupCall === null ? [] : activeGroupCall.seats.map((seat) => seat.userId);
  const profiles = useProfiles(ids);
  // Names resolved once, here, so the pure half renders strings and nothing else: the same split
  // the call screen keeps between its connected and pure halves.
  const names = new Map<Id, string>();
  for (const [id, profile] of profiles) {
    names.set(id, profile.displayName ?? profile.username ?? 'Migo member');
  }
  // The remote streams, read while rendering the seated call — every link change re-renders this
  // half, so a stream that just arrived is an element the same render attaches.
  const streams = new Map<Id, MediaStream>();
  if (activeGroupCall !== null && activeGroupCall.note === null) {
    for (const seat of activeGroupCall.seats) {
      const stream = groupRemoteStream(seat.deviceId);
      if (stream !== null) {
        streams.set(seat.deviceId, stream);
      }
    }
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
      streams={streams}
      onLeave={() => void leaveGroupCall()}
      onDismiss={dismissGroupCall}
      onToggleMute={() => toggleGroupMute()}
      onToggleCamera={() => toggleGroupCamera()}
      onToggleScreenShare={toggleGroupScreenShare}
    />
  );
}
