'use client';

/**
 * The Home section: the realtime dashboard.
 *
 * Home is a glance, not a destination — every block states a fact the session already knows or
 * can read in one round trip, and every row is a door into the section that owns it. The
 * dashboard reads once on mount (balance, suggestions, the notification inbox's first page, the
 * leaderboard's top three, the room catalogue's liveliest page); everything that moves after
 * that moves because the shared providers pushed it — the conversation list reorders and badges
 * live, the room counters tick with the room registry's deltas, and the notification rows land
 * through the inbox's own stream.
 *
 * The blocks are compact by contract: one row per fact, no card chrome around a single number,
 * and the section headings name what each block is a digest of.
 */

import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type {
  Id,
  InboxItem,
  IncomingMessage,
  RankWire,
  RoomSummary,
  SuggestedUser,
  WalletView,
} from '@migo/sdk';

import { conversationTitle } from '@/lib/conversation-title.js';
import { formatRelative } from '@/lib/format.js';
import { messagePreview } from '@/lib/message-preview.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import { useJoinRoom } from '@/lib/migo/use-join-room.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile, useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { CoinMark } from './icons.js';
import { Icon } from './icons.js';
import type { IconName } from './icons.js';
import { EmptyState } from './states.js';
import { Skeleton } from './states.js';
import { Spinner } from './spinner.js';
import { UserProfileModal } from './user-profile-modal.js';

/** How many of each digest the dashboard shows — a glance, not a page. */
const RECENT_CHATS = 4;
const RECENT_ROOMS = 3;
const SUGGESTED_PEOPLE = 4;
const NOTIFICATION_PREVIEW = 4;
const LEADERBOARD_TOP = 3;
const TRENDING_ROOMS = 4;

/**
 * The Home dashboard.
 *
 * @param onOpenSection Switches the shell to a section (the digest rows' doors).
 * @param onOpenConversation Hands an opened conversation to the shell (chats and rooms rows).
 */
export function HomePanel({
  onOpenSection,
  onOpenConversation,
}: {
  onOpenSection: (
    tab: 'chats' | 'rooms' | 'notifications' | 'wallet' | 'friends' | 'search',
  ) => void;
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { items: conversations, unread, lastPreviews } = useConversations();
  const { infoFor } = useRooms();
  const { join, joining } = useJoinRoom(onOpenConversation);
  const self = useProfile(accountId);

  const [balance, setBalance] = useState<WalletView | null>(null);
  const [suggestions, setSuggestions] = useState<SuggestedUser[] | null>(null);
  const [notifications, setNotifications] = useState<InboxItem[] | null>(null);
  const [top, setTop] = useState<RankWire[] | null>(null);
  const [trending, setTrending] = useState<RoomSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Id | null>(null);

  // The one-shot reads: every block's facts, fetched together on mount. A failure names itself
  // once and the digest blocks render their honest empties — a dashboard that hides its blocks
  // reads as a broken one.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      try {
        const [wallet, suggested, inbox, leaders, roomsPage] = await Promise.all([
          client.economy.getBalance(),
          client.social.suggestions(SUGGESTED_PEOPLE).catch(() => [] as SuggestedUser[]),
          client.notifications
            .listNotifications(NOTIFICATION_PREVIEW)
            .catch(() => [] as InboxItem[]),
          client.economy.getLeaderboard('xp', LEADERBOARD_TOP).catch(() => [] as RankWire[]),
          client.rooms.list(20, {}).catch(() => ({ rooms: [] as RoomSummary[] })),
        ]);
        if (cancelled) {
          return;
        }
        setBalance(wallet);
        setSuggestions(suggested);
        setNotifications(inbox);
        setTop(leaders);
        setTrending(
          [...roomsPage.rooms]
            .sort((a, b) => (b.onlineCount ?? 0) - (a.onlineCount ?? 0))
            .slice(0, TRENDING_ROOMS),
        );
        setError(null);
      } catch (cause) {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  // The inbox preview re-reads on a pushed notification — the same reconcile-don't-trust rule
  // the Notifications section applies, over a smaller page.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.notifications.onNotification(() => {
      void client.notifications
        .listNotifications(NOTIFICATION_PREVIEW)
        .then(setNotifications)
        .catch(() => {});
    });
  }, [client]);

  // The profile map the digest titles resolve through — a 1:1's title is its peer's name.
  const chatPeerIds = useMemo(
    () =>
      [
        ...new Set(
          conversations.flatMap((item) =>
            item.kind === ConversationKind.Direct ? (item.members ?? []) : [],
          ),
        ),
      ].filter((id) => id !== accountId),
    [conversations, accountId],
  );
  const chatProfiles = useProfiles(chatPeerIds);

  // The live digests, straight from the shared providers.
  const recentChats = useMemo(
    () =>
      conversations.filter((item) => item.kind !== ConversationKind.Room).slice(0, RECENT_CHATS),
    [conversations],
  );
  const recentRooms = useMemo(
    () =>
      conversations.filter((item) => item.kind === ConversationKind.Room).slice(0, RECENT_ROOMS),
    [conversations],
  );

  const leaderboardIds = useMemo(() => (top ?? []).map((rank) => rank.accountId), [top]);
  const suggestionIds = useMemo(
    () => (suggestions ?? []).map((person) => person.accountId),
    [suggestions],
  );
  const profiles = useProfiles([...leaderboardIds, ...suggestionIds]);

  return (
    <div className="panel panel-wide home-panel">
      {/* The greeting strip: who you are, what you have, and where you are. */}
      <header className="home-hero">
        <Avatar
          name={self?.displayName ?? 'You'}
          id={accountId ?? 'self'}
          size={44}
          avatarUrl={self?.avatarUrl}
        />
        <div className="home-hero-main">
          <h1 className="home-hero-name">{self?.displayName ?? 'You'}</h1>
          <span className="person-sub">
            {self?.username ? `@${self.username}` : 'Welcome back'}
          </span>
        </div>
        <button
          type="button"
          className="mig-chip"
          title="Open your wallet"
          onClick={() => onOpenSection('wallet')}
        >
          <CoinMark size={14} />
          {balance !== null ? balance.balance.toLocaleString() : '…'}
        </button>
      </header>

      {error !== null ? <p className="form-error">{error}</p> : null}

      {/* The quick actions: the three moves a session most often starts with. */}
      <nav className="home-actions" aria-label="Quick actions">
        <button type="button" className="home-action" onClick={() => onOpenSection('search')}>
          <span className="home-action-icon" aria-hidden="true">
            <Icon name="search" size={20} />
          </span>
          Search
        </button>
        <button type="button" className="home-action" onClick={() => onOpenSection('rooms')}>
          <span className="home-action-icon" aria-hidden="true">
            <Icon name="rooms" size={20} />
          </span>
          Browse rooms
        </button>
        <button type="button" className="home-action" onClick={() => onOpenSection('wallet')}>
          <span className="home-action-icon" aria-hidden="true">
            <Icon name="wallet" size={20} />
          </span>
          Wallet
        </button>
      </nav>

      {/* Recent chats: the conversation list's own top, live. */}
      <section className="panel-section" aria-label="Recent chats">
        <header className="home-section-head">
          <h2 className="panel-heading">Recent chats</h2>
          <button type="button" className="link-btn" onClick={() => onOpenSection('chats')}>
            All chats
          </button>
        </header>
        {recentChats.length === 0 ? (
          <p className="muted">No conversations yet — find someone in Search.</p>
        ) : (
          <ul className="digest-list">
            {recentChats.map((conversation) => {
              const preview = lastPreviews.get(conversation.conversationId);
              return (
                <li key={conversation.conversationId}>
                  <button
                    type="button"
                    className="digest-row"
                    onClick={() => onOpenConversation(conversation.conversationId)}
                  >
                    <Avatar
                      name={conversationTitle(conversation, accountId, chatProfiles)}
                      id={conversation.conversationId}
                      size={32}
                    />
                    <span className="digest-main">
                      <span className="digest-line">
                        <span className="person-name">
                          {conversationTitle(conversation, accountId, chatProfiles)}
                        </span>
                        {unread.has(conversation.conversationId) ? (
                          <span className="unread-dot" aria-label="Unread" />
                        ) : null}
                      </span>
                      <span className="person-sub">
                        {preview
                          ? previewText(preview)
                          : conversation.kind === ConversationKind.Room
                            ? 'Room'
                            : 'Direct chat'}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {/* Your rooms: the joined rooms with their live counters. */}
      {recentRooms.length > 0 ? (
        <section className="panel-section" aria-label="Your rooms">
          <header className="home-section-head">
            <h2 className="panel-heading">Your rooms</h2>
            <button type="button" className="link-btn" onClick={() => onOpenSection('rooms')}>
              Browse
            </button>
          </header>
          <ul className="digest-list">
            {recentRooms.map((conversation) => {
              const info = infoFor(conversation.conversationId);
              return (
                <li key={conversation.conversationId}>
                  <button
                    type="button"
                    className="digest-row"
                    onClick={() => onOpenConversation(conversation.conversationId)}
                  >
                    <Avatar
                      name={conversationTitle(conversation, accountId, chatProfiles, info)}
                      id={conversation.conversationId}
                      size={32}
                    />
                    <span className="digest-main">
                      <span className="person-name">
                        {conversationTitle(conversation, accountId, chatProfiles, info)}
                      </span>
                      <span className="person-sub">
                        {info
                          ? `${(info.memberCount ?? 0).toLocaleString()} members · ${(info.onlineCount ?? 0).toLocaleString()} online`
                          : 'Room'}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}

      {/* Notifications digest: the inbox's first page, live through its stream. */}
      <section className="panel-section" aria-label="Notifications">
        <header className="home-section-head">
          <h2 className="panel-heading">Alerts</h2>
          <button type="button" className="link-btn" onClick={() => onOpenSection('notifications')}>
            View all
          </button>
        </header>
        {notifications === null ? (
          <Skeleton rows={2} />
        ) : notifications.length === 0 ? (
          <p className="muted">You are all caught up.</p>
        ) : (
          <ul className="digest-list">
            {notifications.map((item) => (
              <li key={item.id}>
                <button
                  type="button"
                  className="digest-row"
                  onClick={() => onOpenSection('notifications')}
                >
                  <span className="digest-glyph" aria-hidden="true">
                    <Icon name={notificationIcon(item.kind)} size={20} />
                  </span>
                  <span className="digest-main">
                    <span className="person-name">{notificationLabel(item.kind)}</span>
                    {item.title ? <span className="person-sub">{item.title}</span> : null}
                  </span>
                  <time className="person-note">{formatRelative(item.at)}</time>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* Suggested people: the social graph's own recommendations. */}
      {suggestions !== null && suggestions.length > 0 ? (
        <section className="panel-section" aria-label="Suggested people">
          <header className="home-section-head">
            <h2 className="panel-heading">People to meet</h2>
            <button type="button" className="link-btn" onClick={() => onOpenSection('friends')}>
              Friends
            </button>
          </header>
          <ul className="digest-list">
            {suggestions.map((person) => (
              <li key={person.accountId}>
                <button
                  type="button"
                  className="digest-row"
                  onClick={() => setSelected(person.accountId)}
                >
                  <Avatar name={person.displayName} id={person.accountId} size={32} />
                  <span className="digest-main">
                    <span className="person-name">{person.displayName}</span>
                    <span className="person-sub">
                      @{person.username}
                      {person.mutualFriends > 0 ? ` · ${person.mutualFriends} mutual` : ''}
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {/* Trending rooms: the catalogue's liveliest page, offered for a join. */}
      {trending !== null && trending.length > 0 ? (
        <section className="panel-section" aria-label="Trending rooms">
          <header className="home-section-head">
            <h2 className="panel-heading">Trending rooms</h2>
            <button type="button" className="link-btn" onClick={() => onOpenSection('rooms')}>
              All rooms
            </button>
          </header>
          <ul className="digest-list">
            {trending.map((room) => (
              <li key={room.roomId}>
                <div className="digest-row digest-row-static">
                  <Avatar name={room.name} id={room.roomId} size={32} avatarUrl={room.avatarUrl} />
                  <span className="digest-main">
                    <span className="person-name">{room.name}</span>
                    <span className="person-sub">
                      {(room.onlineCount ?? 0).toLocaleString()} online
                      {room.category ? ` · ${room.category}` : ''}
                    </span>
                  </span>
                  <button
                    type="button"
                    className="btn btn-primary btn-sm"
                    disabled={joining.has(room.roomId)}
                    onClick={() => void join(room)}
                  >
                    {joining.has(room.roomId) ? <Spinner /> : 'Join'}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {/* The leaderboard's top three: community standing at a glance. */}
      {top !== null && top.length > 0 ? (
        <section className="panel-section" aria-label="Leaderboard">
          <header className="home-section-head">
            <h2 className="panel-heading">Top XP</h2>
            <button type="button" className="link-btn" onClick={() => onOpenSection('wallet')}>
              Leaderboard
            </button>
          </header>
          <ol className="digest-list">
            {top.map((rank) => (
              <li key={rank.accountId} className="digest-row digest-row-static">
                <span className="digest-rank">#{rank.position}</span>
                <Avatar
                  name={profiles.get(rank.accountId)?.displayName ?? 'Someone'}
                  id={rank.accountId}
                  size={28}
                />
                <span className="person-name">
                  {profiles.get(rank.accountId)?.displayName ?? 'Someone'}
                </span>
                <span className="person-note">
                  Level {rank.level} · {rank.xp} XP
                </span>
              </li>
            ))}
          </ol>
        </section>
      ) : null}

      {balance === null && notifications === null && suggestions === null ? (
        <EmptyState
          icon="home"
          title="Nothing to show yet."
          hint="Your dashboard fills as you use Migo."
        />
      ) : null}

      {selected !== null ? (
        <UserProfileModal userId={selected} blocked={false} onClose={() => setSelected(null)} />
      ) : null}
    </div>
  );
}

/** The preview line for a digest row: one line of the newest message, clamped. */
function previewText(preview: IncomingMessage): string {
  return messagePreview(preview.content, 48);
}

/** The glyph a notification kind renders as, from the closed server vocabulary. */
function notificationIcon(kind: string): IconName {
  if (kind.includes('friend')) {
    return 'friends';
  }
  if (kind.includes('gift')) {
    return 'gift';
  }
  if (kind.includes('game')) {
    return 'game';
  }
  if (kind.includes('room')) {
    return 'rooms';
  }
  if (kind.includes('mention') || kind.includes('message')) {
    return 'chats';
  }
  return 'bell';
}

/** A notification kind as readable words (`friend_request` → `Friend request`). */
function notificationLabel(kind: string): string {
  const spaced = kind.replaceAll('_', ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}
