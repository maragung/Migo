'use client';

/**
 * The desk's Chat List Mode: one split view instead of a window per conversation.
 *
 * The left quarter is the conversation list — searchable, always visible — and the right three
 * quarters is a single thread pane whose contents follow the conversation the list (or any other
 * door into `openConversation`) selected. No window is minted per conversation and none is
 * draggable: the mode's whole point is that the list stays put while the thread beside it
 * changes, so the pane is a fixed region of the desk the shell positions, not a RetroWindow.
 *
 * The thread itself is the same ChatWindow every other surface mounts — the mode changes how a
 * conversation is presented, never how it is read or written — and with no conversation open the
 * pane says so rather than pretending an empty thread is a conversation.
 */

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { ChatWindow } from './chat-window.js';
import { ConversationList } from './conversation-list.js';
import { Icon } from './icons.js';

export function ChatSplitView({ conversationId }: { conversationId: Id | null }): ReactNode {
  return (
    <div className="chat-split">
      <div className="chat-split-list" aria-label="Chat list">
        <ConversationList searchable />
      </div>
      <div className="chat-split-main" aria-label="Chat window">
        {conversationId !== null ? (
          <ChatWindow conversationId={conversationId} />
        ) : (
          <div className="chat-split-empty">
            <Icon name="chats" size={34} />
            <div className="chat-split-empty-title">No conversation open</div>
            <p className="chat-split-empty-text">
              Pick a chat from the list — it opens here, and the list stays beside it.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
