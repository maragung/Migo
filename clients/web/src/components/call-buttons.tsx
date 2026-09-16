'use client';

/**
 * The call controls in a conversation's header: a 1:1 voice or video call to the peer, and the
 * group call's join.
 *
 * The 1:1 buttons render only where a call has exactly one other participant — the chat header
 * passes the peer's id for a `Direct` conversation and nothing for any other kind, so they appear
 * precisely where the wire's 1:1 call signaling can name a callee. The group controls are the
 * mirror: they render only for a `Group` conversation (rooms are public spaces whose open
 * membership deserves its own pass), and they are a voice/video pair now that the group call's
 * media plane carries both.
 */

import type { ReactNode } from 'react';

import { CallMediaKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import type { CallManagerValue } from '@/lib/migo/call-manager.js';
import type { GroupCallManagerValue } from '@/lib/migo/group-call-manager.js';
import type { InProgressGroupCall } from '@/lib/migo/group-roster.js';

/** What the buttons need from the managers: the one action each performs. */
type StartCall = CallManagerValue['startCall'];
type JoinGroupCall = GroupCallManagerValue['joinGroupCall'];

export function CallButtons({
  conversationId,
  peerId,
  onStartCall,
}: {
  /** The conversation the call belongs to. */
  conversationId: Id;
  /** The other account in the 1:1; `null` renders nothing (not a direct conversation). */
  peerId: Id | null;
  /** Places the call; the manager's, already bound. */
  onStartCall: StartCall;
}): ReactNode {
  if (peerId === null) {
    return null;
  }
  return (
    <div className="call-buttons" role="group" aria-label="Start a call">
      <button
        type="button"
        className="icon-btn call-btn"
        aria-label="Voice call"
        title="Voice call"
        onClick={() => void onStartCall(conversationId, peerId, CallMediaKind.Audio)}
      >
        📞
      </button>
      <button
        type="button"
        className="icon-btn call-btn"
        aria-label="Video call"
        title="Video call"
        onClick={() => void onStartCall(conversationId, peerId, CallMediaKind.Video)}
      >
        🎥
      </button>
    </div>
  );
}

/**
 * The group-call control in a group conversation's header: seat this device in the call's roster.
 *
 * There is no ring to answer — a group call in this build is a roster anyone in the conversation
 * may seat themselves in, so the one action is join, and `conversationId` being null (not a group
 * conversation) renders nothing, the same self-gating the 1:1 buttons keep.
 *
 * When other members are already seated (`inProgress`), the same button names the running call
 * and the count it last had: joining a call in progress and starting one are the same wire
 * action — the join's call id decides which — so the affordance changes its words, not its shape.
 */
export function GroupCallButton({
  conversationId,
  inProgress,
  onJoin,
}: {
  /** The group conversation whose call is joined; `null` renders nothing. */
  conversationId: Id | null;
  /** The call already running in the conversation, when one is and this device is not seated in it. */
  inProgress: InProgressGroupCall | null;
  /** Seats this device in the call; the manager's, already bound. */
  onJoin: JoinGroupCall;
}): ReactNode {
  if (conversationId === null) {
    return null;
  }
  const callId = inProgress === null ? undefined : inProgress.callId;
  const countWords =
    inProgress === null ? '' : ` the ${inProgress.participantCount} already in the roster`;
  return (
    <div className="call-buttons" role="group" aria-label="Join the group call">
      <button
        type="button"
        className="icon-btn call-btn"
        aria-label={
          inProgress === null
            ? 'Join group voice call'
            : `Join group voice call in progress (${inProgress.participantCount})`
        }
        title={`Group voice call — join${countWords || ' the roster'}`}
        onClick={() => void onJoin(conversationId, callId, CallMediaKind.Audio)}
      >
        📞
      </button>
      <button
        type="button"
        className="icon-btn call-btn"
        aria-label={
          inProgress === null
            ? 'Join group video call'
            : `Join group video call in progress (${inProgress.participantCount})`
        }
        title={`Group video call — join${countWords || ' the roster'}`}
        onClick={() => void onJoin(conversationId, callId, CallMediaKind.Video)}
      >
        🎥
      </button>
    </div>
  );
}
