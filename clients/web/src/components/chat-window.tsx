'use client';

import { useEffect } from 'react';
import type { ReactNode } from 'react';
import Link from 'next/link';

import { ConversationKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { useChat } from '@/lib/migo/use-chat.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { presenceLabel, usePresence } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { MessageComposer } from './message-composer.js';
import { MessageList } from './message-list.js';
import { Spinner } from './spinner.js';
import { TypingIndicator } from './typing-indicator.js';

export function ChatWindow({ conversationId }: { conversationId: Id }): ReactNode {
  const { client, accountId } = useMigo();
  const { items, markRead } = useConversations();
  const { messages, loading, error, typingUser, send, setTyping } = useChat(conversationId);

  const summary = items.find((item) => item.conversationId === conversationId);
  const isDirect = summary?.kind === ConversationKind.Direct;
  const members = summary?.members ?? [];
  const peerId = isDirect ? (members.find((member) => member !== accountId) ?? null) : null;

  const profiles = useProfiles(peerId ? [peerId] : []);
  const presenceMap = usePresence();

  // Follow the peer's presence for a 1:1 conversation.
  useEffect(() => {
    if (client && peerId) {
      void client.watchUser(peerId).catch(() => {});
    }
  }, [client, peerId]);

  // Clear the unread mark while this conversation is open.
  useEffect(() => {
    markRead(conversationId);
  }, [conversationId, messages.length, markRead]);

  const peerProfile = peerId ? (profiles.get(peerId) ?? null) : null;
  const presence = peerId ? (presenceMap.get(peerId) ?? peerProfile?.presence) : undefined;

  const title = isDirect
    ? (peerProfile?.displayName ?? 'Direct message')
    : (summary?.title ?? (summary?.kind === ConversationKind.Room ? 'Room' : 'Conversation'));
  const subtitle = isDirect ? presenceLabel(presence) : `${members.length || 0} members`;
  const encrypted =
    summary?.kind === ConversationKind.Direct || summary?.kind === ConversationKind.Group;
  const avatarId = (peerId ?? conversationId) as string;

  return (
    <div className="thread-pane" style={{ height: '100%' }}>
      <header className="thread-header">
        <Link href="/chat" className="icon-btn back" aria-label="Back">
          ‹
        </Link>
        <Avatar
          name={title}
          id={avatarId}
          size={38}
          avatarUrl={peerProfile?.avatarUrl}
          presence={presence}
        />
        <div className="thread-heading">
          <div className="name">{title}</div>
          <div className="status">{subtitle}</div>
        </div>
        {encrypted ? (
          <span className="thread-lock" title="Messages are end-to-end encrypted">
            🔒 End-to-end encrypted
          </span>
        ) : null}
      </header>

      {loading && messages.length === 0 ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : error ? (
        <div className="center-fill">
          <div>
            <div className="emoji">⚠️</div>
            {error}
          </div>
        </div>
      ) : accountId ? (
        <MessageList messages={messages} selfId={accountId} />
      ) : null}

      <TypingIndicator userId={typingUser} />
      <MessageComposer onSend={send} onTyping={setTyping} disabled={!!error} />
    </div>
  );
}
