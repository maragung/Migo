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
 * # The quality indicator
 *
 * Section 180 asks for a network-quality indicator on both kinds of call, and it is the tier the
 * manager measured rather than a word this component invents — see {@link ./call-quality.ts}. A
 * voice call shows it too: a lossy link is a fact about the call whether or not a camera is on it,
 * and a user who cannot tell a bad line from a silent peer hangs up on a call that was working.
 * Before the first measurement there is no indicator at all, because a screen that opens on
 * "Excellent" has reported something it never checked.
 *
 * # Media never touches markup it did not come from
 *
 * The video elements attach only the streams the manager owns; nothing about the call — SDP,
 * candidates, stats — is ever rendered or logged. The self-view is muted: hearing your own
 * echo in the overlay is a bug every call UI ships once.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { CallEndReason, CallMediaKind, CallRating, CallState } from '@migo/sdk';
import type { ActiveCall, CallInviteEvent, Id } from '@migo/sdk';

import { applyOutputDevice } from '@/lib/migo/call-devices.js';
import type { CallDevice } from '@/lib/migo/call-devices.js';
import {
  elementPipActive,
  enterElementPip,
  exitElementPip,
  openPipWindow,
  pipMode,
  pipWindowSize,
} from '@/lib/migo/call-pip.js';

import {
  callMediaKindOf,
  callStateLabel,
  displayStateOf,
  endedReasonLine,
  formatCallDuration,
  mediaKindLabel,
} from '@/lib/migo/call-signal.js';
import { QUALITY_CEILINGS, qualityTierLabel } from '@/lib/migo/call-quality.js';
import {
  CALL_ISSUE_KINDS,
  CALL_RATING_CHOICES,
  callIssueLabel,
  callRatingLabel,
} from '@/lib/migo/call-rating.js';
import type { CallIssueKind } from '@/lib/migo/call-rating.js';
import type { LinkQuality } from '@/lib/migo/group-media.js';
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
  /**
   * Whether this side's camera is on. A video call that started with a camera starts with this
   * true; it is false for every voice call, where there is no camera to publish.
   */
  cameraOn: boolean;
  /** Whether a connected call's quality has paused video (never true for a voice call). */
  degraded: boolean;
  /**
   * The rung the call's link is on, or null before the first measurement. Null shows no indicator
   * at all rather than a good one: a screen that says "Excellent" before measuring anything is
   * guessing, and an indicator that guesses is worse than no indicator.
   */
  quality: LinkQuality | null;
  /**
   * Whether this side is sharing its screen. Section 180 makes the indicator a requirement rather
   * than a nicety: a share nobody remembers is the commonest data leak in practice, so the sharer
   * keeps reading that it is sharing for as long as it lasts.
   */
  sharingScreen: boolean;
  /**
   * The captured screen while a share runs. The self-view shows this rather than the camera, since
   * a self-view of the user's own face during a screen share says the opposite of what is going on.
   */
  screenStream: MediaStream | null;
  /**
   * The audio outputs the platform offers. Section 180 names speaker, earpiece, Bluetooth, and
   * wired headset; this is that list as reported, and an empty one draws no speaker control.
   */
  outputs: CallDevice[];
  /**
   * The cameras the platform offers. Fewer than two draws no camera-switch control — a device with
   * one camera has nothing to switch to, and the button would be one that lies about what it did.
   */
  cameras: CallDevice[];
  /**
   * The microphones the platform offers. Unlike outputs this list is never empty on a call that is
   * running — the call is already holding one — so the control is drawn whenever there is more than
   * one to choose between, and a device with a single microphone gets no menu that could only ever
   * restate what is already happening.
   */
  microphones: CallDevice[];
  /** The audio output in use, or null for the platform's default. */
  outputId: string | null;
  /**
   * The microphone in use, or null for the platform's own choice. It is the device the call actually
   * opened rather than the one that was asked for, so the menu names what the peer is hearing.
   */
  inputId: string | null;
  /**
   * Whether this browser can float a call at all. Section 180 asks for picture-in-picture, and the
   * two mechanisms that provide it are not equally useful — the capability is decided by
   * {@link ./call-pip.ts} and reported here as a fact, so the screen draws no control on a browser
   * that could not honour it.
   */
  pipAvailable: boolean;
  /** Whether this call is floating right now, so the control offers the way back rather than out. */
  pipActive: boolean;
  /**
   * Moves the call into a floating window, or takes it out of one. The remote video element is
   * handed over because the fallback mechanism floats that element itself — the browser moves it,
   * and the page cannot substitute one of its own.
   */
  onTogglePip: (video: HTMLVideoElement | null) => void;
  /**
   * The tier the user pinned the call to, or null while the ladder is automatic. Section 180 asks
   * for manual selection alongside the automatic one, and manual is a ceiling: the call can still
   * descend when its link demands it, so this is what the user is willing to spend rather than a
   * promise about what the call will get.
   */
  qualityCeiling: LinkQuality | null;
  /** Whether the low-bandwidth mode is on, which section 180 asks for on voice and video alike. */
  lowBandwidth: boolean;
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
  onToggleCamera: () => void;
  onSwitchCamera: () => void;
  onToggleScreenShare: () => void;
  onSelectOutput: (deviceId: string | null) => void;
  onSelectInput: (deviceId: string | null) => void;
  onSelectQuality: (ceiling: LinkQuality | null) => void;
  onToggleLowBandwidth: (on: boolean) => void;
  /**
   * What the user has said about a call that has just ended, and the three ways they say it.
   * Optional so a screen with no rating state — every state but Ended — needs nothing passed;
   * the default is the prompt's opening state, which is also the state of an unrated call.
   */
  rating?: CallRatingState;
  onPickRating?: (rating: CallRating) => void;
  onToggleRatingIssue?: (issue: CallIssueKind) => void;
  onSubmitRating?: () => void;
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
  cameraOn,
  degraded,
  quality,
  sharingScreen,
  screenStream,
  outputs,
  cameras,
  microphones,
  outputId,
  inputId,
  pipAvailable,
  pipActive,
  qualityCeiling,
  lowBandwidth,
  nowMs,
  endedAt,
  localStream,
  remoteStream,
  onAccept,
  onDecline,
  onCancel,
  onEnd,
  onToggleMute,
  onToggleCamera,
  onSwitchCamera,
  onToggleScreenShare,
  onSelectOutput,
  onSelectInput,
  onSelectQuality,
  onToggleLowBandwidth,
  onTogglePip,
  rating = EMPTY_CALL_RATING,
  onPickRating,
  onToggleRatingIssue,
  onSubmitRating,
  onDismiss,
}: CallScreenProps): ReactNode {
  // The output is applied to the media element rather than carried on the stream: the sink belongs
  // to the element and has to be re-applied whenever the chosen device moves. The ref is a callback
  // rather than an object because of that — React re-runs it on every change of identity, which is
  // exactly the "this element's output just moved" case. A rejection is a device that disappeared
  // between the menu opening and the press, and the call keeps playing where it was.
  //
  // The same element is kept in an object ref as well, for the one caller that has to hand the
  // element itself to the browser rather than a stream: the fallback picture-in-picture mechanism
  // floats that element, and the page cannot substitute one of its own.
  const remoteElement = useRef<HTMLVideoElement | null>(null);
  const remoteRef = useCallback(
    (element: HTMLVideoElement | null): void => {
      remoteElement.current = element;
      if (element === null) {
        return;
      }
      element.srcObject = remoteStream;
      void applyOutputDevice(element, outputId).catch(() => {});
    },
    [remoteStream, outputId],
  );
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
            ref={remoteRef}
          />
          <video
            className="call-video local"
            autoPlay
            playsInline
            muted
            aria-label={
              sharingScreen ? 'The screen you are sharing' : cameraOn ? 'Your video' : 'Camera off'
            }
            ref={(element: HTMLVideoElement | null): void => {
              if (element !== null) {
                // A camera that is off has nothing to show, so the element carries no stream rather
                // than a black frame: a self-view that keeps rendering a disabled track looks like a
                // broken camera, which is a different fact from a camera the user turned off.
                element.srcObject = sharingScreen ? screenStream : cameraOn ? localStream : null;
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
        {display === 'ended' && durationMs !== null ? (
          // The post-call rating, and the reason it is drawn from the duration rather than from the
          // state alone: a call that rang out, was declined, or was cancelled before it connected is
          // one nobody experienced, and asking how it went would collect opinions about a telephone
          // not being answered. A connected call has a duration, and that is the whole test.
          <CallRatingPrompt
            rating={rating}
            onPick={onPickRating ?? ((): void => {})}
            onToggleIssue={onToggleRatingIssue ?? ((): void => {})}
            onSubmit={onSubmitRating ?? ((): void => {})}
          />
        ) : null}
        {quality !== null &&
        (display === 'connected' || display === 'degraded' || display === 'reconnecting') ? (
          // The network indicator: the same word for a voice call and a video call, because a
          // lossy link is a fact about the call whether or not a camera is on it. Polite, so a
          // rung that moves is announced without interrupting the state the screen is reading.
          <div className={`call-quality ${quality}`} aria-live="polite">
            {qualityTierLabel(quality)}
          </div>
        ) : null}
        {sharingScreen ? (
          // The sharer's own reminder, and the reason it is not a toast: section 180 asks for an
          // indicator visible *for as long as sharing runs*, so it stays for the whole share,
          // survives to the Degraded state, and never fades — a share nobody remembers is the
          // commonest data leak in practice.
          <div className="call-sharing" role="status">
            You are sharing your screen
          </div>
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
            {isVideo ? (
              <button
                type="button"
                className={`call-action camera${cameraOn ? '' : ' off'}`}
                aria-label={cameraOn ? 'Turn camera off' : 'Turn camera on'}
                onClick={onToggleCamera}
              >
                {cameraOn ? '📷' : '🚫'}
              </button>
            ) : null}
            {isVideo && cameras.length > 1 ? (
              // Only drawn where the platform reported a second camera: section 180 asks for the
              // front/back switch on the devices that have one, and a control that did nothing on a
              // laptop with a single webcam would be worse than none.
              <button
                type="button"
                className="call-action switch-camera"
                aria-label="Switch camera"
                onClick={onSwitchCamera}
              >
                🔄
              </button>
            ) : null}
            {microphones.length > 1 ? (
              // The input side of the same requirement. Drawn only where the platform reports more
              // than one, because a menu with a single row cannot change anything — and unlike the
              // output list this one always contains the device the call is already using, so the
              // empty option is the platform's own choice rather than the only possibility.
              <select
                className="call-action input"
                value={inputId ?? ''}
                aria-label="Microphone"
                onChange={(event) =>
                  onSelectInput(event.target.value === '' ? null : event.target.value)
                }
              >
                <option value="">System default</option>
                {microphones.map((microphone) => (
                  <option key={microphone.id} value={microphone.id}>
                    {microphone.label}
                  </option>
                ))}
              </select>
            ) : null}
            {outputs.length > 0 ? (
              // Section 180's speaker, earpiece, Bluetooth, and wired headset, as whatever the
              // platform reported. A select rather than a cycling button, because the list is named
              // and the user is choosing between names; the empty option is the system default,
              // which is where a browser that just lost the chosen headset lands.
              <select
                className="call-action output"
                value={outputId ?? ''}
                aria-label="Audio output"
                onChange={(event) =>
                  onSelectOutput(event.target.value === '' ? null : event.target.value)
                }
              >
                <option value="">System default</option>
                {outputs.map((output) => (
                  <option key={output.id} value={output.id}>
                    {output.label}
                  </option>
                ))}
              </select>
            ) : null}
            {isVideo ? (
              // Only a video call gets the tier menu: a ceiling on a voice call would cap a ladder
              // that has no video to act on, and the only thing it could change is the word on the
              // screen — a control whose whole effect is to make the indicator less true.
              //
              // The list is the ladder minus its bottom rung. A user who wants no video has the
              // camera button, which says so plainly; a ceiling of "video off" would instead put the
              // call into Degraded, a state section 180 defines as video paused because quality
              // dropped, and a sacrifice the user chose is not a drop.
              <select
                className="call-action quality"
                value={qualityCeiling ?? ''}
                aria-label="Call quality"
                onChange={(event) =>
                  onSelectQuality(
                    event.target.value === '' ? null : (event.target.value as LinkQuality),
                  )
                }
              >
                <option value="">Automatic</option>
                {QUALITY_CEILINGS.map((ceiling) => (
                  <option key={ceiling} value={ceiling}>
                    {qualityTierLabel(ceiling)}
                  </option>
                ))}
              </select>
            ) : null}
            <button
              type="button"
              className={`call-action low-bandwidth${lowBandwidth ? ' on' : ''}`}
              aria-label={lowBandwidth ? 'Leave low bandwidth mode' : 'Use low bandwidth mode'}
              aria-pressed={lowBandwidth}
              onClick={() => onToggleLowBandwidth(!lowBandwidth)}
            >
              {lowBandwidth ? '🐢' : '🐇'}
            </button>
            {isVideo ? (
              // Only a video call gets the control, because only a video call has a video m-line for
              // a share to ride: section 180 makes screen sharing a video-call capability, and a
              // button that could not do anything is worse than no button.
              <button
                type="button"
                className={`call-action share${sharingScreen ? ' sharing' : ''}`}
                aria-label={sharingScreen ? 'Stop sharing your screen' : 'Share your screen'}
                onClick={onToggleScreenShare}
              >
                🖥️
              </button>
            ) : null}
            {pipAvailable ? (
              // Drawn on a voice call as well as a video call, because the floating window is a
              // placement of the call and not of its picture: a voice call floated keeps its name,
              // its timer, and its controls in front of whatever else the user is doing, which is
              // the whole point of the requirement on a call that has nothing to show.
              <button
                type="button"
                className={`call-action pip${pipActive ? ' on' : ''}`}
                aria-label={pipActive ? 'Leave picture in picture' : 'Picture in picture'}
                aria-pressed={pipActive}
                onClick={() => onTogglePip(remoteElement.current)}
              >
                🖼️
              </button>
            ) : null}
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
 * What the user has said about the call so far, held by the overlay and rendered by the screen.
 *
 * The state lives outside {@link CallScreen} for the same reason every other piece of call state
 * does: the screen is the pure half, so a test can pin what a half-answered prompt looks like
 * without a call and without a server.
 */
export interface CallRatingState {
  /** The verdict picked, or null while the user has picked none. */
  choice: CallRating | null;
  /** The problems ticked. Optional, and a set rather than a choice — a call can have had two. */
  issues: readonly CallIssueKind[];
  /** Whether it has gone to the server, which turns the prompt into its acknowledgement. */
  sent: boolean;
}

/** The prompt in its opening state, which is also the state a call that was never rated is in. */
export const EMPTY_CALL_RATING: CallRatingState = { choice: null, issues: [], sent: false };

export interface CallRatingPromptProps {
  /** What the user has said so far. */
  rating: CallRatingState;
  /** Picks a verdict, or clears it when given the one already picked. */
  onPick: (rating: CallRating) => void;
  /** Ticks or unticks one problem. */
  onToggleIssue: (issue: CallIssueKind) => void;
  /** Sends the verdict and whatever problems are ticked with it. */
  onSubmit: () => void;
}

/**
 * The post-call rating: section 180's four verdicts, and the optional note of what was wrong.
 *
 * It is a separate component because it has a life of its own after the call is over — the screen
 * around it has stopped showing media and stopped ticking — and because the two halves of it are
 * two different kinds of question. The verdict is a choice and exactly one of four, which is what
 * `radiogroup` means and why it is not four toggle buttons; the note is a set, optional, and
 * none-of-the-above is a real answer, which is why it is four toggle buttons and not a second
 * radiogroup.
 *
 * The note is drawn only once a verdict is picked, because the verdict is the thing being asked
 * for and the note is a detail about it: a screen that opened on eight controls would be asking a
 * user who wants to say "good" to first decide whether anything was wrong. Sending needs no note —
 * a verdict on its own is a complete answer, and the commonest one.
 *
 * What it asks for is never call content. Four words and four tick boxes is the whole of what
 * leaves the device, and the copy says so, because a user being asked how a call went is entitled
 * to know what of it is being sent.
 */
export function CallRatingPrompt({
  rating,
  onPick,
  onToggleIssue,
  onSubmit,
}: CallRatingPromptProps): ReactNode {
  if (rating.sent) {
    return (
      <div className="call-rating done" role="status">
        Thanks — your rating was sent.
      </div>
    );
  }
  return (
    <div className="call-rating" aria-label="Rate this call">
      <div className="call-rating-ask">How was the call?</div>
      <div className="call-rating-choices" role="radiogroup" aria-label="Call quality">
        {CALL_RATING_CHOICES.map((choice) => (
          <button
            key={choice}
            type="button"
            role="radio"
            aria-checked={rating.choice === choice}
            className={`call-rating-choice${rating.choice === choice ? ' on' : ''}`}
            onClick={() => onPick(choice)}
          >
            {callRatingLabel(choice)}
          </button>
        ))}
      </div>
      {rating.choice !== null ? (
        <>
          <div className="call-rating-issues" role="group" aria-label="What went wrong">
            {CALL_ISSUE_KINDS.map((issue) => (
              <button
                key={issue}
                type="button"
                aria-pressed={rating.issues.includes(issue)}
                className={`call-rating-issue${rating.issues.includes(issue) ? ' on' : ''}`}
                onClick={() => onToggleIssue(issue)}
              >
                {callIssueLabel(issue)}
              </button>
            ))}
          </div>
          <button type="button" className="btn btn-primary call-rating-send" onClick={onSubmit}>
            Send rating
          </button>
        </>
      ) : null}
      <div className="call-rating-note">
        Only your rating and what you tick is sent. Never what was said or shown.
      </div>
    </div>
  );
}

/**
 * The call, rendered into its own floating window.
 *
 * A Document Picture-in-Picture window is a real window with a real document, and that document is
 * not this page: no stylesheet of the app reaches it, and there is no shell, no chat list and no
 * overlay behind it. So this card brings what it needs — a style block of its own as its first
 * child, and the peer's picture or avatar as its body — and everything else it shows is what the
 * user would otherwise have to come back to the tab to see: who the call is with, how long it has
 * run, whether the microphone is muted, and the controls that act on it — muting, the camera, and
 * hanging up — because a floating call the user has to come back to the tab to end is not a call
 * that has been moved out of the tab.
 *
 * It is a pure component rendering into whatever document it is portalled into, which is what makes
 * it pinnable by a test: the window itself cannot be opened in a test runner, but the markup a
 * window would receive can be rendered and read.
 */
export interface CallPipCardProps {
  /** The peer's display name, the card's one identity line. */
  peerName: string;
  /** The peer's id, for the avatar's stable colour. */
  peerId: string;
  /** The peer's avatar image, when their profile has one. */
  peerAvatarUrl?: string;
  /** Whether this is a video call, which decides picture or avatar. */
  isVideo: boolean;
  /** Whether this side's camera is on; a video call with it off shows the peer's picture only. */
  cameraOn: boolean;
  /** Whether this side's microphone is muted, so the control can say which way it goes. */
  muted: boolean;
  /** Whether this side is sharing its screen, which the sharer must keep being told. */
  sharingScreen: boolean;
  /** The call state as a word — the same word the full screen shows, not a second vocabulary. */
  statusLabel: string;
  /** The running duration as M:SS, or null before the call has one. */
  durationLabel: string | null;
  /** The peer's stream, which the floating picture plays. */
  remoteStream: MediaStream | null;
  /** This side's own stream, for the small self-view a video call with a camera on gets. */
  localStream: MediaStream | null;
  onToggleMute: () => void;
  onToggleCamera: () => void;
  onEnd: () => void;
  /** Takes the call back into the page: closes the window without ending the call. */
  onClose: () => void;
}

export function CallPipCard({
  peerName,
  peerId,
  peerAvatarUrl,
  isVideo,
  cameraOn,
  muted,
  sharingScreen,
  statusLabel,
  durationLabel,
  remoteStream,
  localStream,
  onToggleMute,
  onToggleCamera,
  onEnd,
  onClose,
}: CallPipCardProps): ReactNode {
  return (
    <div className="pip-card">
      {/* The window's document has no stylesheet of its own, so the card carries one. It is scoped
          to this card and written for a window the size of a thumbnail: no sidebar widths, no
          desktop breakpoints, nothing that assumes the shell is behind it. */}
      <style>{PIP_CARD_STYLES}</style>
      <div className="pip-video">
        {isVideo ? (
          <video
            className="pip-remote"
            autoPlay
            playsInline
            aria-label={`${peerName}’s video`}
            ref={(element: HTMLVideoElement | null): void => {
              if (element !== null) {
                element.srcObject = remoteStream;
              }
            }}
          />
        ) : (
          // A voice call has no picture, so the window shows who it is with. This is the only case
          // the avatar covers: a video call with the peer's camera off is still a video call, and
          // the element is left to render whatever the peer is actually publishing.
          <div className="pip-avatar">
            <Avatar name={peerName} id={peerId} size={56} avatarUrl={peerAvatarUrl} />
          </div>
        )}
        {isVideo && cameraOn ? (
          // The self-view, small and in the corner, for the same reason the full screen has one:
          // the user cannot otherwise tell whether their own camera is publishing.
          <video
            className="pip-local"
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
        ) : null}
      </div>
      <div className="pip-identity">
        <div className="pip-name">{peerName}</div>
        <div className="pip-status" aria-live="polite">
          {statusLabel}
          {durationLabel !== null ? ` · ${durationLabel}` : ''}
        </div>
        {sharingScreen ? (
          // The sharer's reminder, carried into the floating window rather than left behind in the
          // tab: a share the user has stopped looking at is exactly the one they forget.
          <div className="pip-sharing" role="status">
            Sharing your screen
          </div>
        ) : null}
      </div>
      <div className="pip-actions">
        <button
          type="button"
          className={`pip-action mute${muted ? ' muted' : ''}`}
          aria-label={muted ? 'Unmute microphone' : 'Mute microphone'}
          onClick={onToggleMute}
        >
          {muted ? '🔇' : '🎙️'}
        </button>
        {isVideo ? (
          <button
            type="button"
            className={`pip-action camera${cameraOn ? '' : ' off'}`}
            aria-label={cameraOn ? 'Turn camera off' : 'Turn camera on'}
            onClick={onToggleCamera}
          >
            {cameraOn ? '📷' : '🚫'}
          </button>
        ) : null}
        <button type="button" className="pip-action hang-up" aria-label="End call" onClick={onEnd}>
          ✕
        </button>
        <button
          type="button"
          className="pip-action"
          aria-label="Back to the call"
          onClick={onClose}
        >
          ⤢
        </button>
      </div>
    </div>
  );
}

/**
 * The floating card's stylesheet, as text because it is written into another document.
 *
 * Kept beside the card rather than in the app's stylesheet for the same reason: the app's
 * stylesheet never reaches a picture-in-picture window, so a rule written there would look correct
 * in review and do nothing at runtime.
 */
const PIP_CARD_STYLES = `
  :root { color-scheme: dark; }
  body { margin: 0; background: #0b0f14; color: #e6edf3;
         font: 13px/1.4 system-ui, -apple-system, "Segoe UI", sans-serif; }
  .pip-card { display: flex; flex-direction: column; height: 100vh; }
  .pip-video { position: relative; flex: 1; min-height: 0; background: #05080b; }
  .pip-remote { width: 100%; height: 100%; object-fit: contain; display: block; }
  .pip-local { position: absolute; right: 8px; bottom: 8px; width: 84px; border-radius: 6px; }
  .pip-avatar { position: absolute; inset: 0; display: flex; align-items: center;
                justify-content: center; }
  .pip-identity { padding: 6px 10px; min-width: 0; }
  .pip-name { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .pip-status { color: #9aa7b4; font-size: 12px; }
  .pip-sharing { color: #f0b429; font-size: 12px; }
  .pip-actions { display: flex; gap: 8px; padding: 0 10px 10px; }
  .pip-action { flex: 1; border: 0; border-radius: 8px; padding: 6px 0; font-size: 15px;
                background: #1b2430; color: inherit; cursor: pointer; }
  .pip-action.muted, .pip-action.off { background: #7f1d1d; }
  .pip-action.hang-up { background: #b91c1c; }
`;

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
    cameraOn,
    degraded,
    quality,
    sharingScreen,
    screenStream,
    outputs,
    cameras,
    microphones,
    outputId,
    inputId,
    qualityCeiling,
    lowBandwidth,
    localStream,
    remoteStream,
    endedAt,
    callError,
    acceptCall,
    declineCall,
    cancelCall,
    endCall,
    toggleMute,
    toggleCamera,
    switchCamera,
    setOutputDevice,
    setInputDevice,
    setQualityCeiling,
    setLowBandwidth,
    toggleScreenShare,
    dismissCall,
    rateCall,
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

  // The floating window, when this call has one. Held as the window rather than as a boolean,
  // because the window is what the card's markup is portalled into and what has to be closed on the
  // way out; the fallback mechanism floats an element instead and reports itself through the
  // browser's own events, so the two are tracked apart.
  const [pipWindow, setPipWindow] = useState<Window | null>(null);
  const [floatingElement, setFloatingElement] = useState<boolean>(false);
  // The capability is read in an effect rather than during render for the same reason the audio
  // output list is: this shell is exported as static HTML, and a control that exists on the client
  // and not in the server's markup is a hydration mismatch rather than a feature.
  const [pipAvailable, setPipAvailable] = useState<boolean>(false);
  const [rating, setRating] = useState<CallRatingState>(EMPTY_CALL_RATING);

  useEffect(() => {
    setPipAvailable(pipMode() !== 'none');
  }, []);

  useEffect(() => {
    // A rating belongs to the call it was given for. A second call must open on its own blank
    // prompt rather than on the previous call's verdict, and because the frame names the call it is
    // about, a choice carried over would be a rating of the wrong call — sent, and wrong.
    setRating(EMPTY_CALL_RATING);
  }, [activeCall?.callId]);

  /**
   * Picks a verdict, or clears the one already picked.
   *
   * Clearing matters for the same reason the prompt can be dismissed: a user who picked Poor by
   * mistake and then decides the call was fine must be able to say so, and a set of radio buttons
   * that could only be changed to a different verdict would make the first press a commitment.
   */
  const pickRating = useCallback((choice: CallRating): void => {
    setRating((current) =>
      current.choice === choice ? { ...current, choice: null, issues: [] } : { ...current, choice },
    );
  }, []);

  const toggleRatingIssue = useCallback((issue: CallIssueKind): void => {
    setRating((current) => ({
      ...current,
      issues: current.issues.includes(issue)
        ? current.issues.filter((held) => held !== issue)
        : [...current.issues, issue],
    }));
  }, []);

  const submitRating = useCallback((): void => {
    if (rating.choice === null) {
      return;
    }
    // The send is outside the state updater deliberately: React may run an updater more than once,
    // and a frame sent from inside one would be sent twice for a single press.
    rateCall(rating.choice, rating.issues);
    // Marked sent before the frame lands, because CALL_STATS is Droppable and has no acknowledgement
    // to wait for: what the screen acknowledges is the user's own act of saying it, and a pending
    // state that could only clear on a frame nobody confirms would be a state that never clears.
    setRating((current) => ({ ...current, sent: true }));
  }, [rateCall, rating]);

  useEffect(() => {
    if (pipWindow === null) {
      return;
    }
    // The user closed the window, with its own close button or the browser's. The call is not
    // affected — a floating window is a placement of the call, not the call — so this only has to
    // stop the overlay claiming to be floating.
    const onClosed = (): void => setPipWindow(null);
    pipWindow.addEventListener('pagehide', onClosed);
    return () => pipWindow.removeEventListener('pagehide', onClosed);
  }, [pipWindow]);

  useEffect(() => {
    // No call left to float: a window that outlived its call would be a peer's picture with no way
    // to hang up a call that has already ended.
    if (activeCall === null && pipWindow !== null) {
      pipWindow.close();
      setPipWindow(null);
    }
  }, [activeCall, pipWindow]);

  const togglePip = useCallback(
    async (video: HTMLVideoElement | null): Promise<void> => {
      if (pipWindow !== null) {
        pipWindow.close();
        setPipWindow(null);
        return;
      }
      if (elementPipActive()) {
        await exitElementPip();
        return;
      }
      // The document window first, because it is the only one of the two that carries the call's
      // controls; the element fallback is what a browser without it is left with.
      if (pipMode() === 'document') {
        const opened = await openPipWindow(
          pipWindowSize(activeCall?.mediaKind === CallMediaKind.Video),
        );
        if (opened !== null) {
          setPipWindow(opened);
        }
        return;
      }
      // The float can end without this client asking — the user closes the browser's window, or the
      // call ends and the element leaves the document — so the state is cleared from the element's
      // own event rather than left claiming a call is still floating.
      if (await enterElementPip(video, () => setFloatingElement(false))) {
        setFloatingElement(true);
      }
    },
    [pipWindow, activeCall],
  );

  if (incomingCall !== null || activeCall !== null) {
    const pipActive = pipWindow !== null || floatingElement;
    const floatingCall = activeCall;
    return (
      <>
        <CallScreen
          call={activeCall}
          incoming={incomingCall}
          peerName={peerName}
          peerId={peerId ?? 'peer'}
          peerAvatarUrl={profile?.avatarUrl}
          muted={muted}
          cameraOn={cameraOn}
          degraded={degraded}
          quality={quality}
          sharingScreen={sharingScreen}
          screenStream={screenStream}
          outputs={outputs}
          cameras={cameras}
          microphones={microphones}
          outputId={outputId}
          inputId={inputId}
          pipAvailable={pipAvailable}
          pipActive={pipActive}
          qualityCeiling={qualityCeiling}
          lowBandwidth={lowBandwidth}
          nowMs={nowMs}
          endedAt={endedAt}
          localStream={localStream}
          remoteStream={remoteStream}
          onAccept={() => void acceptCall()}
          onDecline={() => void declineCall()}
          onCancel={() => void cancelCall()}
          onEnd={(reason) => void endCall(reason)}
          onToggleMute={toggleMute}
          onToggleCamera={toggleCamera}
          onSwitchCamera={() => void switchCamera()}
          onToggleScreenShare={() => void toggleScreenShare()}
          onSelectOutput={setOutputDevice}
          onSelectInput={(deviceId) => void setInputDevice(deviceId)}
          onSelectQuality={setQualityCeiling}
          onToggleLowBandwidth={setLowBandwidth}
          onTogglePip={(video) => void togglePip(video)}
          rating={rating}
          onPickRating={pickRating}
          onToggleRatingIssue={toggleRatingIssue}
          onSubmitRating={submitRating}
          onDismiss={dismissCall}
        />
        {pipWindow !== null && floatingCall !== null
          ? createPortal(
              // The floating window's whole content, rendered into its own document. It is the same
              // call and the same handlers as the screen behind it, so the two views cannot
              // disagree about anything: there is one piece of state and two places showing it.
              <CallPipCard
                peerName={peerName}
                peerId={peerId ?? 'peer'}
                peerAvatarUrl={profile?.avatarUrl}
                isVideo={floatingCall.mediaKind === CallMediaKind.Video}
                cameraOn={cameraOn}
                muted={muted}
                sharingScreen={sharingScreen}
                statusLabel={callStateLabel(displayStateOf(floatingCall, degraded))}
                durationLabel={
                  floatingCall.startedAt !== undefined
                    ? formatCallDuration((endedAt ?? nowMs) - floatingCall.startedAt)
                    : null
                }
                remoteStream={remoteStream}
                localStream={localStream}
                onToggleMute={toggleMute}
                onToggleCamera={toggleCamera}
                onEnd={() =>
                  void endCall(
                    floatingCall.isCaller ? CallEndReason.ByCaller : CallEndReason.ByCallee,
                  )
                }
                onClose={() => {
                  pipWindow.close();
                  setPipWindow(null);
                }}
              />,
              pipWindow.document.body,
            )
          : null}
      </>
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
