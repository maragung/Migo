'use client';

/**
 * The call screen: everything a user sees while a call is ringing, connected, or just ended.
 *
 * The component splits in two, the way the gift and game surfaces do. {@link CallOverlay} is the
 * thin context-connected half — it reads the call manager, resolves the peer's name, and keeps a
 * one-second clock ticking while connected. {@link CallScreen} is the pure half every state of the
 * call renders through, so each screen state is pinnable by a test without a peer connection or a
 * context.
 *
 * # The six states (section 180)
 *
 * A call screen must never go silent-with-no-explanation — a user who cannot tell ringing from
 * dead hangs up and redials. So every state names itself: *Ringing* ("Calling…" out,
 * "Incoming voice call" in), *Connecting*, *Connected* with a running duration, *Reconnecting*
 * while the transport blips, *Degraded* while quality holds video back, and *Ended* always with
 * the reason — a declined call, a failed call, and a network death are different facts, and
 * calling them all "Call ended" throws away the one thing the user needs before calling back.
 *
 * # Media never touches markup it did not come from
 *
 * The video elements attach only the streams the manager owns; nothing about the call — SDP,
 * candidates, stats — is ever rendered or logged. The self-view is muted: hearing your own
 * echo in the overlay is a bug every call UI ships once.
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { CallEndReason, CallMediaKind, CallState } from '@migo/sdk';
import type { ActiveCall, CallInviteEvent, Id } from '@migo/sdk';

import {
  callMediaKindOf,
  callStateLabel,
  displayStateOf,
  endedReasonLine,
  formatCallDuration,
  mediaKindLabel,
} from '@/lib/migo/call-signal.js';
import { useCall } from '@/lib/migo/call-manager.js';
import { MISSED_CALL_MESSAGE } from '@/lib/migo/call-manager.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';

/** Everything the pure call screen needs; every callback is the manager's, already bound. */
export interface CallScreenProps {
  /** The tracked call, including one that just ended. Null while an invite is only ringing. */
  call: ActiveCall | null;
  /** A ringing inbound call nobody has answered; takes the screen over any tracked call. */
  incoming: CallInviteEvent | null;
  /** The peer's display name, resolved by the context-connected half. */
  peerName: string;
  /** The peer's id, for the avatar's stable colour. */
  peerId: string;
  /** The peer's avatar image, when their profile has one. */
  peerAvatarUrl?: string;
  /** Whether this side's microphone is muted. */
  muted: boolean;
  /** Whether a connected call's quality has paused video (always false in this build). */
  degraded: boolean;
  /** The clock the duration reads, passed in so the pure half has no timer of its own. */
  nowMs: number;
  /** When the tracked call ended, for the ended screen's total duration. */
  endedAt: number | null;
  /** This side's own media, for the small self-view on a video call. */
  localStream: MediaStream | null;
  /** The peer's media, for the main view once it arrives. */
  remoteStream: MediaStream | null;
  onAccept: () => void;
  onDecline: () => void;
  onCancel: () => void;
  onEnd: (reason: CallEndReason) => void;
  onToggleMute: () => void;
  onDismiss: () => void;
}

/**
 * The call screen, pure. Renders nothing when there is no call, no invite — the overlay's absence
 * *is* the "no call" state.
 */
export function CallScreen({
  call,
  incoming,
  peerName,
  peerId,
  peerAvatarUrl,
  muted,
  degraded,
  nowMs,
  endedAt,
  localStream,
  remoteStream,
  onAccept,
  onDecline,
  onCancel,
  onEnd,
  onToggleMute,
  onDismiss,
}: CallScreenProps): ReactNode {
  if (incoming !== null) {
    const kind = mediaKindLabel(callMediaKindOf(incoming.mediaKind));
    return (
      <div className="call-overlay" role="dialog" aria-modal="true" aria-label={`Incoming ${kind}`}>
        <div className="call-identity">
          <Avatar name={peerName} id={peerId} size={88} avatarUrl={peerAvatarUrl} />
          <div className="call-name">{peerName}</div>
          <div className="call-status" aria-live="polite">
            Incoming {kind}
          </div>
        </div>
        <div className="call-actions">
          <button
            type="button"
            className="call-action accept"
            aria-label={`Accept ${kind}`}
            onClick={onAccept}
          >
            {callMediaKindOf(incoming.mediaKind) === CallMediaKind.Video ? '📹' : '📞'}
          </button>
          <button
            type="button"
            className="call-action hang-up"
            aria-label="Decline call"
            onClick={onDecline}
          >
            ✕
          </button>
        </div>
      </div>
    );
  }

  if (call === null) {
    return null;
  }

  const display = displayStateOf(call, degraded);
  const isVideo = call.mediaKind === CallMediaKind.Video;
  const showVideos =
    isVideo && (display === 'connected' || display === 'reconnecting' || display === 'degraded');
  const hangUpReason = call.isCaller ? CallEndReason.ByCaller : CallEndReason.ByCallee;
  const durationMs = call.startedAt !== undefined ? (endedAt ?? nowMs) - call.startedAt : null;

  return (
    <div
      className="call-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={`Call with ${peerName}`}
    >
      {showVideos ? (
        <div className="call-video-stage">
          <video
            className="call-video remote"
            autoPlay
            playsInline
            aria-label={`${peerName}\u2019s video`}
            ref={(element: HTMLVideoElement | null): void => {
              if (element !== null) {
                element.srcObject = remoteStream;
              }
            }}
          />
          <video
            className="call-video local"
            autoPlay
            playsInline
            muted
            aria-label="Your video"
            ref={(element: HTMLVideoElement | null): void => {
              if (element !== null) {
                element.srcObject = localStream;
              }
            }}
          />
        </div>
      ) : null}

      <div className="call-identity">
        {!showVideos ? (
          <Avatar name={peerName} id={peerId} size={88} avatarUrl={peerAvatarUrl} />
        ) : null}
        <div className="call-name">{peerName}</div>
        {display === 'ended' ? (
          <div className="call-reason" aria-live="polite">
            {endedReasonLine(call)}
          </div>
        ) : (
          <div className="call-status" aria-live="polite">
            {call.isCaller && display === 'ringing' ? 'Calling…' : callStateLabel(display)}
          </div>
        )}
        {(display === 'connected' || display === 'degraded') && durationMs !== null ? (
          <div className="call-timer" role="timer">
            {formatCallDuration(durationMs)}
          </div>
        ) : null}
        {display === 'ended' && durationMs !== null ? (
          <div className="call-timer">{formatCallDuration(durationMs)}</div>
        ) : null}
      </div>

      <div className="call-actions">
        {display === 'ringing' && call.isCaller ? (
          <button
            type="button"
            className="call-action hang-up"
            aria-label="Cancel call"
            onClick={onCancel}
          >
            ✕
          </button>
        ) : null}
        {display === 'ringing' && !call.isCaller ? (
          <button
            type="button"
            className="call-action hang-up"
            aria-label="End call"
            onClick={() => onEnd(hangUpReason)}
          >
            ✕
          </button>
        ) : null}
        {display === 'connecting' || display === 'reconnecting' ? (
          <button
            type="button"
            className="call-action hang-up"
            aria-label="End call"
            onClick={() => onEnd(hangUpReason)}
          >
            ✕
          </button>
        ) : null}
        {display === 'connected' || display === 'degraded' ? (
          <>
            <button
              type="button"
              className={`call-action mute${muted ? ' muted' : ''}`}
              aria-label={muted ? 'Unmute microphone' : 'Mute microphone'}
              onClick={onToggleMute}
            >
              {muted ? '🔇' : '🎙️'}
            </button>
            <button
              type="button"
              className="call-action hang-up"
              aria-label="End call"
              onClick={() => onEnd(hangUpReason)}
            >
              ✕
            </button>
          </>
        ) : null}
        {display === 'ended' ? (
          <button type="button" className="btn btn-ghost" onClick={onDismiss}>
            Back to chats
          </button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * The small card for a call that could not even be placed: the fact, and a way past it. The
 * label distinguishes the one message that is a notice rather than a failure — the missed-call
 * note the manager leaves when an inbound ring retires — so a screen reader names the dialog
 * for what it is.
 */
export function CallErrorCard({
  message,
  onDismiss,
  label = 'Call failed',
}: {
  message: string;
  onDismiss: () => void;
  /** The dialog's accessible name; defaults to the placement-failure reading. */
  label?: string;
}): ReactNode {
  return (
    <div className="call-overlay error" role="alertdialog" aria-modal="true" aria-label={label}>
      <div className="call-identity">
        <div className="emoji" aria-hidden="true">
          <Icon name="shield" size={24} />
        </div>
        <div className="call-status">{message}</div>
      </div>
      <div className="call-actions">
        <button type="button" className="btn btn-ghost" onClick={onDismiss}>
          Close
        </button>
      </div>
    </div>
  );
}

/**
 * The context-connected half: renders over the whole shell whenever a call is ringing, live, just
 * ended, or failed to start, and nothing at all otherwise.
 */
export function CallOverlay(): ReactNode {
  const {
    activeCall,
    incomingCall,
    muted,
    degraded,
    localStream,
    remoteStream,
    endedAt,
    callError,
    acceptCall,
    declineCall,
    cancelCall,
    endCall,
    toggleMute,
    dismissCall,
  } = useCall();

  const peerId: Id | null = incomingCall
    ? incomingCall.callerId
    : activeCall
      ? activeCall.isCaller
        ? activeCall.calleeId
        : activeCall.callerId
      : null;
  const profiles = useProfiles(peerId !== null ? [peerId] : []);
  const profile = peerId !== null ? (profiles.get(peerId) ?? null) : null;
  const peerName = profile?.displayName ?? profile?.username ?? 'Migo member';

  // One tick per second while connected: the duration is the only number on screen that moves.
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  useEffect(() => {
    if (activeCall === null || activeCall.state !== CallState.Connected) {
      return;
    }
    // Re-zero on entering connected, so the first shown second is this call's, not the mount's.
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [activeCall]);

  if (incomingCall !== null || activeCall !== null) {
    return (
      <CallScreen
        call={activeCall}
        incoming={incomingCall}
        peerName={peerName}
        peerId={peerId ?? 'peer'}
        peerAvatarUrl={profile?.avatarUrl}
        muted={muted}
        degraded={degraded}
        nowMs={nowMs}
        endedAt={endedAt}
        localStream={localStream}
        remoteStream={remoteStream}
        onAccept={() => void acceptCall()}
        onDecline={() => void declineCall()}
        onCancel={() => void cancelCall()}
        onEnd={(reason) => void endCall(reason)}
        onToggleMute={toggleMute}
        onDismiss={dismissCall}
      />
    );
  }

  if (callError !== null) {
    // The missed-call note is a notice about a call that ended elsewhere, not a placement
    // failure; the label keeps the two facts apart for a screen reader.
    return (
      <CallErrorCard
        message={callError}
        onDismiss={dismissCall}
        label={callError === MISSED_CALL_MESSAGE ? MISSED_CALL_MESSAGE : undefined}
      />
    );
  }
  return null;
}
