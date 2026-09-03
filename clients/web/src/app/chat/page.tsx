'use client';

import type { ReactNode } from 'react';

import { ChatWindow } from '@/components/chat-window.js';
import { useOpenConversation } from '@/lib/migo/use-open-conversation.js';

/**
 * The thread pane: the open conversation.
 *
 * One route, not one route per conversation. The bundle is statically exported, so there is no server
 * to render `/chat/<id>` and no build-time list of conversation ids to prerender from. Which
 * conversation is open lives in the URL fragment instead, which also keeps it out of any static host's
 * access log — see `use-open-conversation.ts`.
 *
 * There is no empty state to draw here: the shell only mounts this page when a conversation is
 * open, and shows the pane's own resting content otherwise — an "empty thread" branch would be
 * a second, unreachable answer to the question the shell already answers.
 */
export default function ChatPage(): ReactNode {
  const conversationId = useOpenConversation();
  if (conversationId !== null) {
    return <ChatWindow conversationId={conversationId} />;
  }
  return null;
}
