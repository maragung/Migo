'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, ContentType, EncryptionMode, MemberChange } from '@migo/sdk';
import type {
  ConversationSummary,
  GiftListing,
  Id,
  MediaRefContent,
  VoiceNoteRefContent,
} from '@migo/sdk';

import { messagePreview } from '@/lib/message-preview.js';
import {
  buildConversationLog,
  downloadTextFile,
  formatTranscriptText,
  logFileName,
} from '@/lib/chat-logs.js';
import type { ConversationLog } from '@/lib/chat-logs.js';
import { isAutoSaveEnabled, storeChatLogSnapshot } from '@/lib/storage/chat-log-store.js';
import { useCall } from '@/lib/migo/call-manager.js';
import { useChat } from '@/lib/migo/use-chat.js';
import { useGameEvents } from '@/lib/migo/use-game-events.js';
import { useRoomNotices } from '@/lib/migo/use-room-notices.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useSafety } from '@/lib/migo/safety.js';
import { useSectionNav } from '@/lib/migo/section-nav.js';
import { useGroupNotices } from '@/lib/migo/use-group-notices.js';
import { useRooms, capacityLabel } from '@/lib/migo/rooms-provider.js';
import { useMuted, muteFilter } from '@/lib/migo/muted-provider.js';
import { resolveMediaObject } from '@/lib/migo/media.js';
import { presenceLabel, usePresence } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { closeConversation } from '@/lib/migo/use-open-conversation.js';
import { useOwnedPacks } from '@/lib/migo/use-owned-packs.js';

import { Avatar } from './avatar.js';
import { CallButtons } from './call-buttons.js';
import { DirectInfoPanel, SafetyWarningBannerView } from './direct-info-panel.js';
import { EmoticonPicker } from './emoticon-picker.js';
import { GameEventList } from './game-events.js';
import { GameLauncher } from './game-launcher.js';
import { GiftPicker } from './gift-picker.js';
import { GroupInfoPanel } from './group-info-panel.js';
import { DISAPPEARING_MS, MessageComposer } from './message-composer.js';
import { MessageList, senderNameOf } from './message-list.js';
import type { InterleavedRow } from './message-list.js';
import { RoomInfoPanel } from './room-info-panel.js';
import { RoomNoticeLine } from './room-notice-line.js';
import { Icon } from './icons.js';
import { Spinner } from './spinner.js';
import { TypingIndicator } from './typing-indicator.js';
import { UserProfileModal } from './user-profile-modal.js';

/** How much of the message being replied to the composer's preview bar quotes. */
const REPLY_PREVIEW_CHARS = 50;

/** How many gifts the composer's inline picker offers. */
const GIFT_PICKER_COUNT = 6;

/**
 * How often the auto-save tick runs while a conversation is open.
 *
 * Two triggers share one path: the tick catches a crash or a killed tab mid-conversation, and the
 * teardown write (which also fires on a conversation switch, because the effect re-runs) catches
 * the ordinary end of a read. Both are cheap — one IndexedDB put over state already in memory —
 * so the interval does not need to be clever about when the transcript "changed enough".
 */
const AUTOSAVE_INTERVAL_MS = 2 * 60 * 1000;

/**
 * A fresh idempotency key for one gift-picker session, from the platform CSPRNG.
 *
 * The key rides on every send attempt from that session so the server can tell a
 * retry of the same intent (a lost reply, a re-tap) from a second, separate gift.
 */
function newIntentKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

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

  const summary = items.find((item) => item.conversationId === conversationId);
  // Whether this thread's media is sealed before upload. Every direct conversation and group is
  // end-to-end by construction; a public or managed room is `Transport`, and its content policy
  // is the server's, so attachments there keep the legacy plaintext path (see `lib/migo/media.js`).
  // A summary not yet loaded is treated as end-to-end — the honest default for the conversation
  // kinds that carry no encryption field, and rooms always arrive with theirs.
  const endToEnd = summary === undefined || summary.encryption === EncryptionMode.EndToEnd;

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
    expiresAfterMs,
    setExpiresAfterMs,
  } = useChat(conversationId, { endToEnd });
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
  // The 1:1's details — the safety numbers — behind the same ⓘ again. The read that observes the
  // peer's identities runs on conversation open (not panel open), because a changed identity key
  // must be visible whether or not the person ever looks at the numbers; the panel is where the
  // change is reviewed and acknowledged.
  const [safetyOpen, setSafetyOpen] = useState(false);
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
  // One idempotency key per picker session: minted when the picker opens, sent with
  // every gift attempt from it, so a retry after a lost reply returns the first send
  // instead of charging the sender twice. Closing the picker ends the intent.
  const [giftKey, setGiftKey] = useState<string | null>(null);
  // The composer's emoticon/sticker picker, beside the gift picker it shares the row with.
  const [emoticonOpen, setEmoticonOpen] = useState(false);
  // The account's purchased packs: one read per session, shared across every chat window.
  const ownedPacks = useOwnedPacks(client);
  // The handle the picker inserts through; the composer fills it on mount. A ref rather than
  // state because it is a stable function the composer owns, not a value the tree renders.
  const emoticonInputRef = useRef<{ insert: (glyph: string) => void } | null>(null);

  /**
   * The media resolver the message list embeds images and plays voice notes through: download,
   * decrypt with the message's key slots (or pass legacy plaintext through), object URL. A failure
   * resolves to `null` rather than rejecting, so one unopenable object degrades to its placeholder
   * instead of taking the render path down; the session-wide cache behind it lives in
   * `lib/migo/media.js`.
   */
  const mediaObjectFor = useCallback(
    async (content: MediaRefContent | VoiceNoteRefContent): Promise<string | null> => {
      if (!client) {
        return null;
      }
      try {
        return await resolveMediaObject(client, content);
      } catch {
        return null;
      }
    },
    [client],
  );
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
  // The pair safety numbers for this 1:1, one per device the peer publishes: read on open (the
  // observation point for the key-change warning), cached in the SDK for this client's life so the
  // panel costs no further prekeys. Idle for a non-direct or a note to self.
  const safety = useSafety(conversationId, peerId);
  // The room behind this conversation, when the shell knows one (from this session's joins, or
  // the account's remembered rooms): the header's live counters and topic come from it, because
  // the conversation summary carries neither.
  const roomInfo = rooms.infoFor(conversationId);

  // The open room's live membership pills — who joined, left, dropped, or was removed — kept only
  // for the room on screen and interleaved into the transcript at the moment each happened.
  const roomNotices = useRoomNotices(roomInfo?.roomId ?? null);
  // The open group's own membership pills, from the same-shaped stream a room uses.
  const groupNotices = useGroupNotices(isGroup ? conversationId : null);
  // The two tails as interleaved rows: each membership change becomes a system line the message
  // list places in time order among the bubbles, instead of a pile accumulating under the thread.
  // The keys carry the stream they came from because each tail numbers its own arrivals.
  const noticeRows = useMemo<InterleavedRow[]>(() => {
    const rows: InterleavedRow[] = [];
    for (const notice of roomNotices) {
      rows.push({
        at: notice.at,
        key: `room-${notice.seq}`,
        node: <RoomNoticeLine notice={notice} />,
      });
    }
    for (const notice of groupNotices) {
      rows.push({
        at: notice.at,
        key: `group-${notice.seq}`,
        node: <RoomNoticeLine notice={notice} place="group" />,
      });
    }
    return rows;
  }, [roomNotices, groupNotices]);

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

  // The transcript this window holds, as a portable log. The header's download and the auto-save
  // tick render the same artifact through the same builder, so the wording in a saved file can
  // never drift from the wording the bubbles show — the log's vocabulary is the preview's
  // (lib/message-preview.js), reached through buildConversationLog. It sits below the title's
  // declaration because it closes over the title; a hoisted read of a `const` is a build error,
  // and this comment is what stops a future move from re-introducing it.
  const buildCurrentLog = useCallback((): ConversationLog | null => {
    if (accountId === null || messages.length === 0) {
      return null;
    }
    return buildConversationLog(conversationId, title, messages, (senderId) =>
      senderNameOf(senderId, accountId, profiles),
    );
  }, [accountId, conversationId, messages, profiles, title]);

  const buildLogRef = useRef(buildCurrentLog);
  buildLogRef.current = buildCurrentLog;

  // The auto-save: when armed (Settings → Chats & Log), a periodic tick and the teardown write
  // both snapshot the current transcript into IndexedDB. The toggle is read at write time, so
  // turning it off takes effect on the next tick with no provider wiring; the ref keeps the
  // interval subscribed to the conversation, not to every message, so a busy room does not churn
  // timers.
  useEffect(() => {
    function snapshotNow(): void {
      if (!isAutoSaveEnabled()) {
        return;
      }
      const log = buildLogRef.current();
      if (log !== null) {
        void storeChatLogSnapshot(log);
      }
    }
    const timer = setInterval(snapshotNow, AUTOSAVE_INTERVAL_MS);
    return () => {
      clearInterval(timer);
      // The teardown write: the effect re-runs on a conversation switch, so this is both "the
      // window closed" and "the reader moved on" — the two moments a snapshot is most wanted.
      snapshotNow();
    };
  }, [conversationId]);

  // The header's download: the transcript as it stands, as a plain-text file.
  const downloadTranscript = useCallback((): void => {
    const log = buildCurrentLog();
    if (log !== null) {
      downloadTextFile(formatTranscriptText(log), logFileName(log.title, 'txt'), 'text/plain');
    }
  }, [buildCurrentLog]);

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
  // to this thread for both ledgers, and the picker-session key rides along so a retry after a
  // lost reply is the first send again server-side.
  const sendGift = useCallback(
    (gift: GiftListing, recipient: Id): void => {
      if (!client || giftBusy) {
        return;
      }
      setGiftBusy(true);
      setGiftError(null);
      client.economy
        .sendGift(gift.sku, recipient, conversationId, giftKey ?? undefined)
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
    [client, giftBusy, conversationId, giftKey],
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

  // The pane is a flex column (see .thread-pane): the transcript takes what is left and the
  // composer stays pinned to the window's bottom edge.
  return (
    <div className="thread-pane">
      <header className="thread-header">
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
        {/* The transcript as a file: plaintext by design, saved only on this device — the same
            honest framing the Settings → Chats & Log group carries. */}
        <button
          type="button"
          className="icon-btn"
          onClick={downloadTranscript}
          aria-label="Download chat log"
          title="Download this conversation as a text file"
        >
          <Icon name="download" size={20} />
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
        {isDirect && peerId !== null ? (
          <button
            type="button"
            className={`icon-btn ${safetyOpen ? 'active' : ''}`}
            onClick={() => setSafetyOpen((open) => !open)}
            aria-label={safetyOpen ? 'Hide safety numbers' : 'Show safety numbers'}
            aria-expanded={safetyOpen}
            title="Safety numbers — verify this conversation"
          >
            ⓘ
          </button>
        ) : null}
        {/* A 1:1 is the one conversation this build can call: the wire's invite names a single
            callee, and a group call needs the SFU this build does not have. */}
        <CallButtons conversationId={conversationId} peerId={peerId} onStartCall={startCall} />
        {supportsGames ? <GameLauncher onStart={game.startGame} /> : null}
      </header>

      {safety.changed ? (
        <SafetyWarningBannerView
          onReview={() => {
            setSafetyOpen(true);
          }}
        />
      ) : null}

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

      {isDirect && safetyOpen ? <DirectInfoPanel safety={safety} /> : null}

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
          mediaObjectFor={mediaObjectFor}
          interleaved={noticeRows}
          liveSlot={
            <>
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
      {emoticonOpen ? (
        <EmoticonPicker
          owned={ownedPacks}
          onInsert={(glyph) => {
            // The picker inserts into the composer's own state through the shared send path:
            // appending to the draft keeps the glyph editable before send, the same as typing it.
            emoticonInputRef.current?.insert(glyph);
          }}
          onClose={() => setEmoticonOpen(false)}
        />
      ) : null}
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
        // File send is a private-and-group feature: in a server-readable room the attach button is
        // hidden entirely (the composer renders no picker when onAttach is undefined), while the mic
        // below stays for every conversation kind.
        onAttach={endToEnd ? sendAttachment : undefined}
        onVoiceNote={sendVoiceNote}
        onTyping={setTyping}
        disabled={!!error}
        replyPreview={replyPreview}
        onCancelReply={() => setReplyTo(null)}
        onGift={() => {
          setEmoticonOpen(false);
          if (!giftOpen) {
            setGiftKey(newIntentKey());
          }
          setGiftOpen(!giftOpen);
        }}
        giftOpen={giftOpen}
        emoticonOpen={emoticonOpen}
        // Disappearing messages are a private-and-group feature: a room's history is its record
        // (the room's transcripts are the point of a room), so the clock control stays out of a
        // room's composer entirely — the same rule that hides file send there.
        expiresAfterMs={isRoom ? null : expiresAfterMs}
        onToggleDisappearing={
          isRoom
            ? undefined
            : () => setExpiresAfterMs(expiresAfterMs == null ? DISAPPEARING_MS : null)
        }
        onToggleEmoticon={() => {
          setGiftOpen(false);
          setEmoticonOpen((open) => !open);
        }}
        insertRef={emoticonInputRef}
      />

      {profileOpen && peerId !== null ? (
        <UserProfileModal userId={peerId} onClose={() => setProfileOpen(false)} />
      ) : null}
    </div>
  );
}
