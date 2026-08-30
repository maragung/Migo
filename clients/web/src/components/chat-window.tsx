'use client';

import { useCallback, useEffect, useMemo } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, EncryptionMode } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { messagePreview } from '@/lib/message-preview.js';
import { useChat } from '@/lib/migo/use-chat.js';
import { useGameEvents } from '@/lib/migo/use-game-events.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import { resolveMediaUrl } from '@/lib/migo/media.js';
import { presenceLabel, usePresence } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { GameEventList } from './game-events.js';
import { GameLauncher } from './game-launcher.js';
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
  const rooms = useRooms();
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
    sendVoiceNote,
  } = useChat(conversationId);
  const game = useGameEvents(conversationId);

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
  const isRoom = summary?.kind === ConversationKind.Room;
  // Games are offered where a game has an audience: groups and rooms. A 1:1 has exactly the two
  // people the wire's GAME_START cannot name as opponents, and a solo game in a private chat is
  // a notification generator, not a pastime.
  const supportsGames =
    summary?.kind === ConversationKind.Group || summary?.kind === ConversationKind.Room;
  const members = summary?.members ?? [];
  const peerId = isDirect ? (members.find((member) => member !== accountId) ?? null) : null;
  // The room behind this conversation, when the shell knows one (from this session's joins, or
  // the account's remembered rooms): the header's live counters and topic come from it, because
  // the conversation summary carries neither.
  const roomInfo = rooms.infoFor(conversationId);

  // Every sender in the thread resolves to a profile (names, avatars, reply quotes), plus the
  // direct peer so the header shows a name even before they have spoken, plus the players of any
  // game seen in the thread, whose names the game rows quote.
  const senderIds = useMemo(() => {
    const ids: Id[] = [];
    const seen = new Set<Id>();
    const push = (id: Id): void => {
      if (!seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    };
    for (const message of messages) {
      push(message.senderId);
    }
    if (peerId !== null) {
      push(peerId);
    }
    for (const row of game.rows) {
      if (row.actorId !== undefined) {
        push(row.actorId);
      }
    }
    for (const view of game.views.values()) {
      for (const player of view.players) {
        push(player);
      }
    }
    return ids;
  }, [messages, peerId, game.rows, game.views]);
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
    : (summary?.title ??
      roomInfo?.name ??
      (summary?.kind === ConversationKind.Room ? 'Room' : 'Conversation'));
  // A room's status line is its live shape — how many are in, how many are here — with the topic
  // as the header's second line when the room states one. Without room info the line is the
  // conversation's own membership, which is the honest fallback for a room the shell has not
  // joined in this session and does not remember.
  const subtitle = isDirect
    ? presenceLabel(presence)
    : isRoom
      ? `${roomInfo?.onlineCount ?? 0} online · ${roomInfo?.memberCount ?? members.length} members`
      : `${members.length || 0} members`;
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
          <div className="name">
            {isRoom ? (
              <span className="room-glyph" aria-hidden="true">
                #
              </span>
            ) : null}
            {title}
          </div>
          <div className="status">{subtitle}</div>
          {isRoom && roomInfo?.topic ? <div className="thread-topic">{roomInfo.topic}</div> : null}
        </div>
        {encryptionLabel ? (
          <span className="thread-lock" title={encryptionLabel}>
            {encryptionLabel}
          </span>
        ) : null}
        {supportsGames ? <GameLauncher onStart={game.startGame} /> : null}
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
          liveSlot={
            <GameEventList
              rows={game.rows}
              views={game.views}
              selfId={accountId}
              profiles={profiles}
              activeGuess={game.activeGuess}
              onSubmitGuess={(value) => void game.submitGuess(value)}
              guessBusy={game.guessBusy}
              guessError={game.guessError}
            />
          }
          liveRowCount={game.rows.length + (game.activeGuess !== null ? 1 : 0)}
        />
      ) : null}

      <TypingIndicator userId={typingUser} />
      <MessageComposer
        onSend={send}
        onAttach={sendAttachment}
        onVoiceNote={sendVoiceNote}
        onTyping={setTyping}
        disabled={!!error}
        replyPreview={replyPreview}
        onCancelReply={() => setReplyTo(null)}
      />
    </div>
  );
}
