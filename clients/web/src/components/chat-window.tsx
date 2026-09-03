'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, ContentType, EncryptionMode, MemberChange } from '@migo/sdk';
import type { ConversationSummary, GiftListing, Id } from '@migo/sdk';

import { messagePreview } from '@/lib/message-preview.js';
import { useCall } from '@/lib/migo/call-manager.js';
import { useChat } from '@/lib/migo/use-chat.js';
import { useGameEvents } from '@/lib/migo/use-game-events.js';
import { useRoomNotices } from '@/lib/migo/use-room-notices.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useSectionNav } from '@/lib/migo/section-nav.js';
import { useGroupNotices } from '@/lib/migo/use-group-notices.js';
import { useRooms, capacityLabel } from '@/lib/migo/rooms-provider.js';
import { useMuted, muteFilter } from '@/lib/migo/muted-provider.js';
import { resolveMediaUrl } from '@/lib/migo/media.js';
import { presenceLabel, usePresence } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { CallButtons } from './call-buttons.js';
import { GameEventList } from './game-events.js';
import { GameLauncher } from './game-launcher.js';
import { GiftPicker } from './gift-picker.js';
import { GroupInfoPanel } from './group-info-panel.js';
import { MessageComposer } from './message-composer.js';
import { MessageList, senderNameOf } from './message-list.js';
import { RoomInfoPanel } from './room-info-panel.js';
import { RoomNoticeList } from './room-notice-list.js';
import { Icon } from './icons.js';
import { Spinner } from './spinner.js';
import { TypingIndicator } from './typing-indicator.js';
import { UserProfileModal } from './user-profile-modal.js';

/** How much of the message being replied to the composer's preview bar quotes. */
const REPLY_PREVIEW_CHARS = 50;

/** How many gifts the composer's inline picker offers. */
const GIFT_PICKER_COUNT = 6;

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

/**
 * The account a call from this thread would dial, or `null` when the thread cannot be called.
 *
 * Pure, so a test can pin the gate. A call is offered only for a `Direct` conversation with a
 * second member: the wire's invite names exactly one callee, and a group or room call is the
 * SFU flow this build does not have. A direct thread whose only member is ourselves (a note to
 * self) has nobody to dial either.
 */
export function callPeerFor(
  summary: ConversationSummary | undefined,
  accountId: Id | null,
): Id | null {
  if (summary === undefined || summary.kind !== ConversationKind.Direct) {
    return null;
  }
  return summary.members?.find((member) => member !== accountId) ?? null;
}

export function ChatWindow({ conversationId }: { conversationId: Id }): ReactNode {
  const { client, accountId } = useMigo();
  const navigate = useSectionNav();
  const { items, markRead, forgetConversation } = useConversations();
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
    editMessage,
    react,
    hasEarlier,
    loadingEarlier,
    loadEarlier,
    sendVoiceNote,
  } = useChat(conversationId);
  const game = useGameEvents(conversationId);
  const { startCall } = useCall();
  const { muted } = useMuted();

  // The thread's overlays: the peer's profile (a direct chat), the room's details (a room), and
  // the composer's gift picker. Each is plain open/closed state over the same conversation.
  const [profileOpen, setProfileOpen] = useState(false);
  const [roomInfoOpen, setRoomInfoOpen] = useState(false);
  // The group's details — roster, invite, mute, kick, vote, rename, leave — behind the same ⓘ the
  // room uses, so a multi-party conversation always has one obvious way "into" its membership.
  const [groupInfoOpen, setGroupInfoOpen] = useState(false);
  // The in-thread search: a filter over the transcript this session already holds. The spec's
  // room header carries a search control; a client-side filter over loaded messages is the
  // honest version of it, and it labels itself when it is only searching what is loaded.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [giftOpen, setGiftOpen] = useState(false);
  const [giftCatalogue, setGiftCatalogue] = useState<GiftListing[] | null>(null);
  const [giftRecipient, setGiftRecipient] = useState<Id | null>(null);
  const [giftBusy, setGiftBusy] = useState(false);
  const [giftError, setGiftError] = useState<string | null>(null);

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
  const isGroup = summary?.kind === ConversationKind.Group;
  // Games are offered where a game has an audience: groups and rooms. A 1:1 has exactly the two
  // people the wire's GAME_START cannot name as opponents, and a solo game in a private chat is
  // a notification generator, not a pastime.
  const supportsGames =
    summary?.kind === ConversationKind.Group || summary?.kind === ConversationKind.Room;
  // A memo, because the fallback's `?? []` would otherwise mint a fresh array per render and
  // make every hook that depends on the membership re-run forever.
  const members = useMemo(() => summary?.members ?? [], [summary]);
  // Personal mute hides a muted account's chatter in *rooms* only — a direct thread is never
  // filtered, however the peer is muted elsewhere. The filter runs over the whole loaded transcript
  // (not just newly-arrived messages), so muting someone clears their backlog from view at once.
  const visibleMessages = useMemo(
    () => (isRoom ? muteFilter(messages, muted) : messages),
    [isRoom, messages, muted],
  );
  const peerId = callPeerFor(summary, accountId);
  // The room behind this conversation, when the shell knows one (from this session's joins, or
  // the account's remembered rooms): the header's live counters and topic come from it, because
  // the conversation summary carries neither.
  const roomInfo = rooms.infoFor(conversationId);

  // The open room's live membership pills — who joined, left, dropped, or was removed — kept only
  // for the room on screen and rendered in the transcript's live region below.
  const roomNotices = useRoomNotices(roomInfo?.roomId ?? null);
  // The open group's own membership pills, from the same-shaped stream a room uses.
  const groupNotices = useGroupNotices(isGroup ? conversationId : null);

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

  // A removal from this group closes the thread: this account can no longer read the group, and a
  // thread it cannot read must not stay on screen. Joined and Reconnected keep the account seated;
  // every other change — a leave of our own (belt to the panel's braces), a kick, a ban, a drop —
  // takes the conversation off the list and the window out of the way. The details panel handles
  // its own closing for the buttons it owns; this is the path for everything else.
  useEffect(() => {
    if (!client || !accountId || !isGroup) {
      return;
    }
    return client.conversations.onMember((event) => {
      if (
        event.conversationId !== conversationId ||
        event.userId !== accountId ||
        event.change === MemberChange.Joined ||
        event.change === MemberChange.Reconnected
      ) {
        return;
      }
      forgetConversation(conversationId);
      closeConversation();
    });
  }, [client, accountId, isGroup, conversationId, forgetConversation]);

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
      ? `${capacityLabel(roomInfo?.onlineCount, roomInfo?.maxMembers)} online · ${roomInfo?.memberCount ?? members.length} members`
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

  // The gift picker's catalogue loads once, on first open — the shop is not worth a fetch for
  // every thread that never sends one.
  useEffect(() => {
    if (!client || !giftOpen || giftCatalogue !== null) {
      return;
    }
    let cancelled = false;
    client.economy
      .getGiftCatalogue()
      .then((catalogue) => {
        if (!cancelled) {
          setGiftCatalogue(catalogue.slice(0, GIFT_PICKER_COUNT));
        }
      })
      .catch(() => {
        if (!cancelled) {
          setGiftCatalogue([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, giftOpen, giftCatalogue]);

  // A gift from the composer: the conversation rides along so the server can attach the transfer
  // to this thread for both ledgers.
  const sendGift = useCallback(
    (gift: GiftListing, recipient: Id): void => {
      if (!client || giftBusy) {
        return;
      }
      setGiftBusy(true);
      setGiftError(null);
      client.economy
        .sendGift(gift.sku, recipient, conversationId)
        .then(() => {
          setGiftOpen(false);
        })
        .catch(() => {
          setGiftError('That gift could not be sent.');
        })
        .finally(() => {
          setGiftBusy(false);
        });
    },
    [client, giftBusy, conversationId],
  );

  // The gift picker's candidate recipients: the conversation's other members with resolved
  // names. A direct chat has exactly one; a room without a known member list offers none, and
  // the picker says so rather than guessing a recipient.
  const giftRecipients = useMemo(() => {
    if (accountId === null) {
      return [];
    }
    return members
      .filter((member) => member !== accountId)
      .map((member) => ({
        id: member,
        name: profiles.get(member)?.displayName ?? 'Someone',
      }));
  }, [members, accountId, profiles]);

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
        {isDirect && peerId !== null ? (
          <button
            type="button"
            className="thread-identity"
            onClick={() => setProfileOpen(true)}
            aria-label={`View ${title}'s profile`}
          >
            <Avatar
              name={title}
              id={peerId}
              size={38}
              avatarUrl={peerProfile?.avatarUrl}
              presence={presence}
            />
            <div className="thread-heading">
              <div className="name">{title}</div>
              <div className="status">{subtitle}</div>
            </div>
          </button>
        ) : (
          <>
            <Avatar name={title} id={avatarId} size={38} />
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
              {isRoom && roomInfo?.topic ? (
                <div className="thread-topic">{roomInfo.topic}</div>
              ) : null}
            </div>
          </>
        )}
        {encryptionLabel ? (
          <span className="thread-lock" title={encryptionLabel}>
            {encryptionLabel}
          </span>
        ) : null}
        <button
          type="button"
          className={`icon-btn ${searchOpen ? 'active' : ''}`}
          onClick={() => {
            setSearchOpen((open) => !open);
            setSearchQuery('');
          }}
          aria-label={searchOpen ? 'Close search' : 'Search this conversation'}
          aria-expanded={searchOpen}
          title="Search this conversation"
        >
          <Icon name="search" size={20} />
        </button>
        {isRoom && roomInfo !== null ? (
          <button
            type="button"
            className="icon-btn"
            onClick={() => setRoomInfoOpen((open) => !open)}
            aria-label={roomInfoOpen ? 'Hide room details' : 'Show room details'}
            aria-expanded={roomInfoOpen}
            title="Room details"
          >
            ⓘ
          </button>
        ) : null}
        {isGroup ? (
          <button
            type="button"
            className={`icon-btn ${groupInfoOpen ? 'active' : ''}`}
            onClick={() => setGroupInfoOpen((open) => !open)}
            aria-label={groupInfoOpen ? 'Hide group details' : 'Show group details'}
            aria-expanded={groupInfoOpen}
            title="Group details — members, invites, mute, kick"
          >
            ⓘ
          </button>
        ) : null}
        {/* A 1:1 is the one conversation this build can call: the wire's invite names a single
            callee, and a group call needs the SFU this build does not have. */}
        <CallButtons conversationId={conversationId} peerId={peerId} onStartCall={startCall} />
        {supportsGames ? <GameLauncher onStart={game.startGame} /> : null}
      </header>

      {searchOpen ? (
        <div className="thread-search">
          <input
            type="search"
            className="input"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            placeholder="Filter loaded messages"
            aria-label="Filter loaded messages"
            autoFocus
          />
        </div>
      ) : null}

      {isRoom && roomInfoOpen && roomInfo !== null ? (
        <RoomInfoPanel roomId={roomInfo.roomId} conversationId={conversationId} />
      ) : null}

      {isGroup && groupInfoOpen ? (
        <GroupInfoPanel conversationId={conversationId} title={summary?.title ?? 'Group'} />
      ) : null}

      {loading && messages.length === 0 ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : error ? (
        <div className="center-fill">
          <div>
            <div className="emoji">
              <Icon name="shield" size={24} />
            </div>
            {error}
          </div>
        </div>
      ) : accountId ? (
        <MessageList
          messages={
            searchQuery.trim().length > 0
              ? visibleMessages.filter((message) => {
                  const content = message.content;
                  return (
                    content.type === ContentType.Text &&
                    content.text.toLowerCase().includes(searchQuery.trim().toLowerCase())
                  );
                })
              : visibleMessages
          }
          selfId={accountId}
          showSenders={showSenders}
          profiles={profiles}
          readUpTo={readUpTo}
          onReply={setReplyTo}
          onDelete={deleteMessage}
          onEdit={(message, text) => editMessage(message.messageId, text)}
          onReact={(message, emoji) => react(message.messageId, emoji)}
          deleting={deleting}
          hasEarlier={hasEarlier}
          loadingEarlier={loadingEarlier}
          onLoadEarlier={loadEarlier}
          mediaUrlFor={mediaUrlFor}
          liveSlot={
            <>
              {isRoom ? <RoomNoticeList notices={roomNotices} /> : null}
              {isGroup ? <RoomNoticeList notices={groupNotices} place="group" /> : null}
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
            </>
          }
          liveRowCount={
            (isRoom ? roomNotices.length : 0) +
            (isGroup ? groupNotices.length : 0) +
            game.rows.length +
            (game.activeGuess !== null ? 1 : 0)
          }
          onOpenWallet={() => navigate('wallet')}
        />
      ) : null}

      <TypingIndicator userId={typingUser} />
      {giftOpen ? (
        giftCatalogue === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : (
          <GiftPicker
            gifts={giftCatalogue}
            recipients={giftRecipients}
            selectedRecipient={giftRecipient}
            onSelectRecipient={setGiftRecipient}
            onSend={sendGift}
            onClose={() => {
              setGiftOpen(false);
              setGiftError(null);
            }}
            busy={giftBusy}
          />
        )
      ) : null}
      {giftError ? <p className="composer-meta composer-error">{giftError}</p> : null}
      <MessageComposer
        onSend={send}
        onAttach={sendAttachment}
        onVoiceNote={sendVoiceNote}
        onTyping={setTyping}
        disabled={!!error}
        replyPreview={replyPreview}
        onCancelReply={() => setReplyTo(null)}
        onGift={() => setGiftOpen((open) => !open)}
        giftOpen={giftOpen}
      />

      {profileOpen && peerId !== null ? (
        <UserProfileModal userId={peerId} onClose={() => setProfileOpen(false)} />
      ) : null}
    </div>
  );
}
