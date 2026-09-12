'use client';

/**
 * The call controls in a conversation's header: a 1:1 voice or video call to the peer, and the
 * group call's join.
 *
 * The 1:1 buttons render only where a call has exactly one other participant — the chat header
 * passes the peer's id for a `Direct` conversation and nothing for any other kind, so they appear
 * precisely where the wire's 1:1 call signaling can name a callee. The group control is the
 * mirror: it renders only for a `Group` conversation (rooms are public spaces whose open
 * membership deserves its own pass), and it is one button, not a voice/video pair — this build
 * carries the roster and no media, and a video button would promise video it cannot render.
 */

import type { ReactNode } from 'react';

import { CallMediaKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import type { CallManagerValue } from '@/lib/migo/call-manager.js';
import type { GroupCallManagerValue } from '@/lib/migo/group-call-manager.js';

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
 * The group-call control in a group conversation's header: join the call's roster.
 *
 * There is no ring to answer — a group call in this build is a roster anyone in the conversation
 * may seat themselves in, so the one action is join, and `conversationId` being null (not a group
 * conversation) renders nothing, the same self-gating the 1:1 buttons keep.
 */
export function GroupCallButton({
  conversationId,
  onJoin,
}: {
  /** The group conversation whose call is joined; `null` renders nothing. */
  conversationId: Id | null;
  /** Seats this device in the call; the manager's, already bound. */
  onJoin: JoinGroupCall;
}): ReactNode {
  if (conversationId === null) {
    return null;
  }
  return (
    <button
      type="button"
      className="icon-btn call-btn"
      aria-label="Join group call"
      title="Group voice call — join the roster"
      onClick={() => void onJoin(conversationId)}
    >
      📞
    </button>
  );
}
