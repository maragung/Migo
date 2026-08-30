'use client';

/**
 * The call controls in a direct conversation's header: place a voice or video call to the peer.
 *
 * Rendered only where a call has exactly one other participant — the chat header passes the peer's
 * id for a `Direct` conversation and nothing for any other kind, so the buttons appear precisely
 * where the wire's 1:1 call signaling can name a callee, and nowhere a group or room call would
 * need an SFU this build does not have.
 */

import type { ReactNode } from 'react';

import { CallMediaKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import type { CallManagerValue } from '@/lib/migo/call-manager.js';

/** What the buttons need from the manager: the one action they perform. */
type StartCall = CallManagerValue['startCall'];

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
