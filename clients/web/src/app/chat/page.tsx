'use client';

import type { ReactNode } from 'react';

import { ChatWindow } from '@/components/chat-window.js';
import { useOpenConversation } from '@/lib/migo/use-open-conversation.js';

/**
 * The thread pane: the open conversation, or the empty state.
 *
 * One route, not one route per conversation. The bundle is statically exported, so there is no server
 * to render `/chat/<id>` and no build-time list of conversation ids to prerender from. Which
 * conversation is open lives in the URL fragment instead, which also keeps it out of any static host's
 * access log — see `use-open-conversation.ts`.
 */
export default function ChatPage(): ReactNode {
  const conversationId = useOpenConversation();
  if (conversationId !== null) {
    return <ChatWindow conversationId={conversationId} />;
  }
  return (
    <div className="empty-thread">
      <div className="empty-thread-inner">
        <div className="emoji">🔒</div>
        <h2>Select a conversation</h2>
        <p>
          Your messages are end-to-end encrypted. Pick a conversation on the left, or start a new
          one.
        </p>
      </div>
    </div>
  );
}
