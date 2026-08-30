'use client';

/**
 * The Space section: the activity stream.
 *
 * Space is Migo's feed, and the feed the wire can honestly offer is the account's own activity:
 * the notification inbox (durable, server-ordered), the wallet's ledger lines (gifts sent and
 * received, stakes and payouts), and the live event streams (friend changes, game moves, room
 * membership and state) that arrive while the session is open. The panel merges the three into
 * one newest-first stream — a notification is the server's durable record of what happened, a
 * live event is the same moment seen in realtime, and a ledger line is the money-side fact —
 * with category filters (All, Social, Rooms, Games, Economy) over the merged stream.
 *
 * The merge rules are deliberate: live events prepend with their receipt time (the wire carries
 * no timestamp for a friend change, and the moment it arrived is the moment it became news), a
 * re-read of the inbox or ledger replaces the durable halves without touching the live ones
 * (they have no durable ids to dedupe by), and every row that names a conversation or a room is
 * a door into it.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { NotificationKind } from '@migo/sdk';
import type { FriendEvent, Id, InboxItem, LedgerEntryWire, NotificationEvent } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Icon } from './icons.js';
import type { IconName } from './icons.js';
import { EmptyState } from './states.js';
import { Skeleton } from './states.js';

/** The durable page sizes: the newest slice of each source, one screen of merged stream. */
const NOTIFICATION_ROWS = 50;
const LEDGER_ROWS = 20;

/** How many live rows cap the stream's realtime half — a feed grows downward, not forever. */
const LIVE_CAP = 30;

/** The stream's categories, each a filter over the merged rows. */
type Category = 'all' | 'social' | 'rooms' | 'games' | 'economy';

const CATEGORIES: ReadonlyArray<{ id: Category; label: string }> = [
  { id: 'all', label: 'All' },
  { id: 'social', label: 'Social' },
  { id: 'rooms', label: 'Rooms' },
  { id: 'games', label: 'Games' },
  { id: 'economy', label: 'Economy' },
];

/** The row the stream renders: one activity, from whichever source produced it. */
interface ActivityRow {
  /** A stable key for React's list reconciliation: the source's own id or a receipt tag. */
  key: string;
  /** The activity's category, for the filters. */
  category: Exclude<Category, 'all'>;
  icon: IconName;
  /** The headline: what happened, in the user's words. */
  title: string;
  /** The moment it happened (or arrived), in epoch ms. */
  at: number;
  /** The conversation the row opens into, when it names one. */
  conversationId?: Id;
  /** The room the row names, as a label only — the wire names no conversation for it here. */
  roomId?: Id;
  /** The actor, resolved through the shared profile cache for the display name. */
  actorId?: Id;
}

/**
 * The Space section panel.
 *
 * @param onOpenConversation Hands an opened conversation to the shell — the stream's rows that
 *   name a conversation are doors into it.
 */
export function SpacePanel({
  onOpenConversation,
}: {
  onOpenConversation: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();

  const [notifications, setNotifications] = useState<InboxItem[] | null>(null);
  const [ledger, setLedger] = useState<LedgerEntryWire[] | null>(null);
  const [live, setLive] = useState<ActivityRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<Category>('all');
  // The reload nonce lets the empty state's Try again re-run the durable read.
  const [reloadNonce, setReloadNonce] = useState(0);
  /** The monotonic tag that keeps live rows' keys unique across a session. */
  const liveSeq = useRef(0);

  // The durable halves: the inbox and the money-side statement, read together on mount. Each
  // half may fail alone (a server without the economy service still streams notifications), so
  // each settles independently — a half that cannot be read simply renders nothing, and the
  // panel's error line is reserved for the case where neither durable half arrived.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    void (async (): Promise<void> => {
      const inbox = client.notifications
        .listNotifications(NOTIFICATION_ROWS)
        .then((items) => {
          if (!cancelled) {
            setNotifications(items);
          }
        })
        .catch(() => {
          if (!cancelled) {
            setNotifications([]);
          }
        });
      const statement = client.economy
        .getLedger(LEDGER_ROWS)
        .then((entries) => {
          if (!cancelled) {
            setLedger(entries);
          }
        })
        .catch(() => {
          if (!cancelled) {
            setLedger([]);
          }
        });
      const settled = await Promise.allSettled([inbox, statement]);
      if (!cancelled && settled.every((result) => result.status === 'rejected')) {
        setError(friendlyError(new Error('Could not load the activity stream.')));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, reloadNonce]);

  // The realtime half: every live event that arrives while the session is open becomes a row at
  // the head of the stream, and the inbox re-reads behind it (the durable record is the truth a
  // pushed event only hints at — the same reconcile rule the Notifications section applies).
  useEffect(() => {
    if (!client) {
      return;
    }
    const prepend = (row: Omit<ActivityRow, 'key'>): void => {
      liveSeq.current += 1;
      setLive((prev) => [{ ...row, key: `live-${liveSeq.current}` }, ...prev].slice(0, LIVE_CAP));
    };

    const offs: Array<() => void> = [
      client.notifications.onNotification((event) => {
        prepend(notificationEventRow(event));
        void client.notifications
          .listNotifications(NOTIFICATION_ROWS)
          .then(setNotifications)
          .catch(() => {});
      }),
      client.social.onFriendEvent((event: FriendEvent) => {
        prepend(friendRow(event));
      }),
      client.games.onGameEvent((event) => {
        prepend({
          category: 'games',
          icon: 'game',
          title: event.text ?? 'A game moved',
          at: Date.now(),
          roomId: event.roomId,
          ...(event.actorId !== undefined ? { actorId: event.actorId } : {}),
        });
      }),
      client.rooms.onMember((event) => {
        prepend({
          category: 'rooms',
          icon: 'rooms',
          title: event.joined ? 'Someone joined a room' : 'Someone left a room',
          at: Date.now(),
          roomId: event.roomId,
          actorId: event.userId,
        });
      }),
    ];
    return () => {
      for (const off of offs) {
        off();
      }
    };
  }, [client]);

  // The merged stream: durable rows (notifications, ledger) plus the live rows, newest first.
  // A ledger line and a notification can describe the same gift — the wire gives them different
  // ids and different words, so both stand: one is the social fact, one is the money fact.
  const rows = useMemo(() => {
    const durable: ActivityRow[] = [];
    for (const item of notifications ?? []) {
      durable.push(notificationRow(item));
    }
    for (const entry of ledger ?? []) {
      durable.push(ledgerRow(entry));
    }
    return [...live, ...durable].sort((left, right) => right.at - left.at);
  }, [notifications, ledger, live]);

  const filtered = useMemo(
    () => (filter === 'all' ? rows : rows.filter((row) => row.category === filter)),
    [rows, filter],
  );

  const actorIds = useMemo(
    () => [...new Set(rows.map((row) => row.actorId).filter((id): id is Id => id !== undefined))],
    [rows],
  );
  const profiles = useProfiles(actorIds);

  return (
    <div className="panel panel-wide">
      <header className="panel-head">
        <h1 className="panel-title">Space</h1>
        <button
          type="button"
          className="icon-btn"
          aria-label="Refresh activity"
          title="Refresh activity"
          onClick={() => setReloadNonce((nonce) => nonce + 1)}
        >
          <Icon name="refresh" size={20} />
        </button>
      </header>

      <div className="chip-row" role="group" aria-label="Filter activity">
        {CATEGORIES.map((option) => (
          <button
            key={option.id}
            type="button"
            className={`chip ${filter === option.id ? 'chip-active' : ''}`}
            aria-pressed={filter === option.id}
            onClick={() => setFilter(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>

      {error !== null ? (
        <EmptyState
          icon="space"
          title={error}
          action={
            <button
              type="button"
              className="btn"
              onClick={() => setReloadNonce((nonce) => nonce + 1)}
            >
              Try again
            </button>
          }
        />
      ) : notifications === null || ledger === null ? (
        <Skeleton rows={5} />
      ) : filtered.length === 0 ? (
        <EmptyState
          icon="space"
          title={filter === 'all' ? 'No activity yet.' : `No ${filter} activity yet.`}
          hint="Your stream fills as your friends, rooms, and games move."
        />
      ) : (
        <ul className="activity-list" aria-label="Activity stream">
          {filtered.map((row) => (
            <ActivityListRow
              key={row.key}
              row={row}
              actorName={
                row.actorId !== undefined ? (profiles.get(row.actorId)?.displayName ?? null) : null
              }
              onOpen={
                row.conversationId !== undefined
                  ? () => onOpenConversation(row.conversationId as Id)
                  : null
              }
            />
          ))}
        </ul>
      )}
    </div>
  );
}

/** One stream row: the glyph, the headline, the actor, and the time. */
function ActivityListRow({
  row,
  actorName,
  onOpen,
}: {
  row: ActivityRow;
  actorName: string | null;
  onOpen: (() => void) | null;
}): ReactNode {
  const title =
    actorName !== null && row.actorId !== undefined && !row.title.startsWith(actorName)
      ? `${actorName} — ${row.title}`
      : row.title;
  const body = (
    <>
      <span className="activity-glyph" data-category={row.category} aria-hidden="true">
        <Icon name={row.icon} size={20} />
      </span>
      <span className="digest-main">
        <span className="person-name">{title}</span>
        <span className="person-sub">{categoryLabel(row.category)}</span>
      </span>
      <time className="person-note" dateTime={new Date(row.at).toISOString()}>
        {formatRelative(row.at)}
      </time>
    </>
  );
  return (
    <li className="activity-row">
      {onOpen !== null ? (
        <button type="button" className="activity-row-btn" onClick={onOpen}>
          {body}
        </button>
      ) : (
        <div className="activity-row-static">{body}</div>
      )}
    </li>
  );
}

/** The category a notification kind belongs to, from the closed server vocabulary. */
function notificationCategory(kind: string): Exclude<Category, 'all'> {
  if (kind.includes('friend')) {
    return 'social';
  }
  if (kind.includes('gift') || kind.includes('ledger') || kind.includes('coin')) {
    return 'economy';
  }
  if (kind.includes('game')) {
    return 'games';
  }
  if (kind.includes('room')) {
    return 'rooms';
  }
  return 'social';
}

/** The glyph a notification kind renders as. */
function notificationIconOf(kind: string): IconName {
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
  return 'bell';
}

/** An inbox item as a stream row. */
function notificationRow(item: InboxItem): ActivityRow {
  const kind = item.kind;
  const spaced = kind.replaceAll('_', ' ');
  return {
    key: `notif-${item.id}`,
    category: notificationCategory(kind),
    icon: notificationIconOf(kind),
    title: spaced.charAt(0).toUpperCase() + spaced.slice(1),
    at: item.at,
    ...(item.conversationId !== undefined ? { conversationId: item.conversationId } : {}),
    ...(item.roomId !== undefined ? { roomId: item.roomId } : {}),
    ...(item.actorId !== undefined ? { actorId: item.actorId } : {}),
  };
}

/**
 * A pushed notification as a stream row.
 *
 * The live event carries the kind as the {@link NotificationKind} enum's number (the inbox row
 * carries it as a snake_case word), so the enum's own name — `FriendRequest` — is spaced into
 * the same words the inbox's `friend_request` renders.
 */
function notificationEventRow(event: NotificationEvent): ActivityRow {
  const kindName = NotificationKind[event.kind] ?? 'Event';
  const spaced = kindName.replaceAll(/([a-z])([A-Z])/g, '$1 $2').toLowerCase();
  const label = spaced.charAt(0).toUpperCase() + spaced.slice(1);
  return {
    key: `notif-live-${event.at}-${event.kind}`,
    category: notificationCategory(spaced.replaceAll(' ', '_')),
    icon: notificationIconOf(spaced.replaceAll(' ', '_')),
    title: event.title ?? label,
    at: event.at,
    ...(event.conversationId !== undefined ? { conversationId: event.conversationId } : {}),
    ...(event.roomId !== undefined ? { roomId: event.roomId } : {}),
    ...(event.actorId !== undefined ? { actorId: event.actorId } : {}),
  };
}

/** A ledger line as a stream row: the money-side fact, signed and dated. */
function ledgerRow(entry: LedgerEntryWire): ActivityRow {
  const label = ledgerReasonLabel(entry.reason);
  const signed = ledgerSigned(entry.reason) ? `+${entry.amount}` : `−${entry.amount}`;
  return {
    key: `ledger-${entry.txId}`,
    category: 'economy',
    icon: 'coins',
    title: `${label} ${signed} $MIG`,
    at: entry.at,
  };
}

/** A ledger reason as readable words (`gift_purchase` → `Gift purchase`). */
function ledgerReasonLabel(reason: string): string {
  const spaced = reason.replaceAll('_', ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/** Whether a ledger reason credits the balance — the same closed mapping the Wallet states. */
function ledgerSigned(reason: string): boolean {
  return (
    reason === 'grant' ||
    reason === 'gift_reputation' ||
    reason === 'refund' ||
    reason === 'game_payout'
  );
}

/**
 * A friend-event as a stream row.
 *
 * The wire's `state` is a closed string vocabulary (`"request"`, `"accepted"`, …) — a hint that
 * the graph moved, not a full statement — so the row states exactly what the event said and no
 * more, the same restraint the Friends panel applies when it re-reads rather than patches.
 */
function friendRow(event: FriendEvent): ActivityRow {
  const label =
    event.state === 'accepted'
      ? 'is now your friend'
      : event.state === 'request'
        ? 'sent you a friend request'
        : event.state === 'removed' || event.state === 'blocked'
          ? 'is no longer connected'
          : 'friend activity';
  return {
    key: `friend-${event.userId}-${Date.now()}`,
    category: 'social',
    icon: 'friends',
    title: label,
    at: Date.now(),
    actorId: event.userId,
  };
}

/** A category as its filter label. */
function categoryLabel(category: Exclude<Category, 'all'>): string {
  return CATEGORIES.find((option) => option.id === category)?.label ?? 'Activity';
}
