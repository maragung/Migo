'use client';

/**
 * The Chat List Mode's chat activity: the thread as a screen of its own.
 *
 * WhatsApp-on-Android shaped, deliberately. The mode's main screen is the conversation list, and
 * a tap on a row opens the thread as a separate full-screen activity — not a pane the list shares
 * a window with, and not a small dialog over it — whose one way out, beside the browser's own
 * Back (the fragment is a real history entry), is the back control the thread's header carries.
 * Back returns to the list; the thread is the same ChatWindow every other surface mounts, so the
 * mode changes how a conversation is presented, never how it is read or written.
 *
 * The frame is a sibling of the desk, outside its stacking context: the desk's z-index fence
 * keeps every window the shell mints below the overlays, and the activity rides at the same
 * height those overlays do — above whatever the desk is showing, below the sheets and confirm
 * dialogs that portal to the body. On a phone it covers the strip too, because the strip is the
 * list's navigation and the list is not on screen; on the desk it leaves the taskbar visible —
 * the taskbar is the desk's own chrome (the clock, the session timer, the connection mark), not
 * this conversation's, and a full-screen activity on a desktop still owes the desk its bar.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { ChatWindow } from './chat-window.js';

export function ChatActivity({
  conversationId,
  onBack,
  deskTaskbar,
}: {
  conversationId: Id;
  /** Returns to the list — the activity's back control, wired by the shell to the fragment. */
  onBack: () => void;
  /**
   * Which edge the desk's taskbar occupies, so the activity leaves the bar visible; `null` on a
   * phone, where the activity covers everything.
   */
  deskTaskbar: 'top' | 'bottom' | null;
}): ReactNode {
  return (
    <div
      className={`chat-activity${
        deskTaskbar === 'top'
          ? ' chat-activity-tb-top'
          : deskTaskbar === 'bottom'
            ? ' chat-activity-tb-bottom'
            : ''
      }`}
    >
      <ChatWindow conversationId={conversationId} onBack={onBack} />
    </div>
  );
}
