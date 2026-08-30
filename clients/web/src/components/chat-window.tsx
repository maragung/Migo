'use client';

import { useCallback, useEffect, useMemo } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, EncryptionMode } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { messagePreview } from '@/lib/message-preview.js';
import { useChat } from '@/lib/migo/use-chat.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { resolveMediaUrl } from '@/lib/migo/media.js';
import { presenceLabel, usePresence } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { MessageComposer } from './message-composer.js';
import { MessageList, senderNameOf } from './message-list.js';
import { Spinner } from './spinner.js';
import { TypingIndicator } from './typing-indicator.js';

/** How much of the message being replied to the composer's preview bar quotes. */
const REPLY_PREVIEW_CHARS = 50;

/**
 * The lock label the header may claim, from the summary's {@link EncryptionMode}.
 *
 * The mode is the server's own statement of what the UI is allowed to claim, so the label is
 * derived from it and never from the conversation kind: kind says who is in the conversation, not
 * how (or whether) the transport protects it. `Unknown` renders no label rather than a guess.
 */
export function encryptionLabelFor(mode: EncryptionMode | undefined): string | null {
  switch (mode) {
    case EncryptionMode.EndToEnd:
      return '🔒 End-to-end encrypted';
    case EncryptionMode.Transport:
      return 'Encrypted transport (server can read for moderation)';
    case EncryptionMode.None:
      return 'Not encrypted';
    default:
      return null;
  }
}

export function ChatWindow({ conversationId }: { conversationId: Id }): ReactNode {
  const { client, accountId } = useMigo();
  const { items, markRead } = useConversations();
  const {
    messages,
    loading,
    error,
    typingUser,
    readUpTo,
    replyTo,
    setReplyTo,
    send,
    sendAttachment,
    setTyping,
    deleting,
    deleteMessage,
    hasEarlier,
    loadingEarlier,
    loadEarlier,
  } = useChat(conversationId);

  /**
   * The media resolver the message list embeds images through. A failure resolves to `null` rather
   * than rejecting, so one unresolvable object degrades to its placeholder instead of taking the
   * render path down; the session-wide cache behind it lives in `lib/migo/media.js`.
   */
  const mediaUrlFor = useCallback(
    async (mediaId: Id): Promise<string | null> => {
      if (!client) {
        return null;
      }
      try {
        return await resolveMediaUrl(client, mediaId);
      } catch {
        return null;
      }
    },
    [client],
  );

  const summary = items.find((item) => item.conversationId === conversationId);
  const isDirect = summary?.kind === ConversationKind.Direct;
  const members = summary?.members ?? [];
  const peerId = isDirect ? (members.find((member) => member !== accountId) ?? null) : null;

  // Every sender in the thread resolves to a profile (names, avatars, reply quotes), plus the
  // direct peer so the header shows a name even before they have spoken.
  const senderIds = useMemo(() => {
    const ids: Id[] = [];
    const seen = new Set<Id>();
    for (const message of messages) {
      if (!seen.has(message.senderId)) {
        seen.add(message.senderId);
        ids.push(message.senderId);
      }
    }
    if (peerId !== null && !seen.has(peerId)) {
      ids.push(peerId);
    }
    return ids;
  }, [messages, peerId]);
  const profiles = useProfiles(senderIds);
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
  const encryptionLabel = encryptionLabelFor(summary?.encryption);
  const avatarId = (peerId ?? conversationId) as string;
  // Sender names and avatars are for multi-party conversations; in a 1:1 the alignment already
  // says who spoke.
  const showSenders = summary !== undefined && !isDirect;

  const replyPreview =
    replyTo && accountId
      ? {
          senderName: senderNameOf(replyTo.senderId, accountId, profiles),
          snippet: replyTo.deleted
            ? '[deleted]'
            : messagePreview(replyTo.content, REPLY_PREVIEW_CHARS),
        }
      : null;

  return (
    <div className="thread-pane" style={{ height: '100%' }}>
      <header className="thread-header">
        <button
          type="button"
          onClick={closeConversation}
          className="icon-btn back"
          aria-label="Back"
        >
          ‹
        </button>
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
        {encryptionLabel ? (
          <span className="thread-lock" title={encryptionLabel}>
            {encryptionLabel}
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
        <MessageList
          messages={messages}
          selfId={accountId}
          showSenders={showSenders}
          profiles={profiles}
          readUpTo={readUpTo}
          onReply={setReplyTo}
          onDelete={deleteMessage}
          deleting={deleting}
          hasEarlier={hasEarlier}
          loadingEarlier={loadingEarlier}
          onLoadEarlier={loadEarlier}
          mediaUrlFor={mediaUrlFor}
        />
      ) : null}

      <TypingIndicator userId={typingUser} />
      <MessageComposer
        onSend={send}
        onAttach={sendAttachment}
        onTyping={setTyping}
        disabled={!!error}
        replyPreview={replyPreview}
        onCancelReply={() => setReplyTo(null)}
      />
    </div>
  );
}
