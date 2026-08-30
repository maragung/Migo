'use client';

import { useMemo } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, MessageKind } from '@migo/sdk';
import type {
  ConversationSummary,
  Id,
  IncomingMessage,
  MessageContent,
  MessageEvent,
} from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { messagePreview, truncate } from '@/lib/message-preview.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import type { RoomInfo } from '@/lib/migo/rooms-provider.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { conversationHref, useOpenConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { Spinner } from './spinner.js';

/** The sidebar's preview line is a fixed-height row; anything longer is noise next to a title. */
const PREVIEW_CHARS = 40;

/** The fixed label for a message kind, used when the sealed body has not been decrypted. */
function placeholderForKind(kind: MessageKind): string {
  switch (kind) {
    case MessageKind.Media:
      return '📎 Attachment';
    case MessageKind.Voice:
      return '🎤 Voice note';
    case MessageKind.Gift:
      return '🎉 Gift';
    case MessageKind.Sticker:
      return 'Sticker';
    case MessageKind.Game:
      return 'Game move';
    case MessageKind.System:
      return 'System message';
    default:
      return 'Message';
  }
}

/**
 * The row's preview line for a conversation's last message, or null when there is nothing to
 * preview (the caller then shows the static subtitle).
 *
 * `decrypted` is the opened body when the provider's replay managed to read the sealed
 * `lastMessage` event; without it the line falls back to the event's cleartext kind, so the row
 * still says what arrived. Groups prefix the sender — `You` for our own — so the line reads like
 * the start of a sentence; a 1:1 has only one other voice, so it goes straight to the body.
 */
export function lastMessagePreviewLine(
  summary: ConversationSummary,
  decrypted: MessageContent | null,
  senderLabel: string | null,
  maxChars: number = PREVIEW_CHARS,
): string | null {
  const event: MessageEvent | undefined = summary.lastMessage;
  if (decrypted === null && event === undefined) {
    return null;
  }
  // A server-side tombstone outranks a stale decrypted preview: the message was unsent, so the
  // row must say so rather than quote what the sender took back.
  const body =
    event?.deleted === true
      ? '[deleted]'
      : decrypted !== null
        ? messagePreview(decrypted, maxChars)
        : placeholderForKind(event?.kind ?? MessageKind.Unknown);
  const prefix =
    summary.kind !== ConversationKind.Direct && senderLabel !== null ? `${senderLabel}: ` : '';
  return truncate(prefix + body, maxChars);
}

/**
 * A room row's title: the `#` glyph and the room's name.
 *
 * The glyph is the row's only claim to be a room before it is opened (the kind drives styling
 * hooks, but the glyph is what a reader sees), and the name prefers the shell's room record over
 * the summary's `title`, which the conversation list leaves unset for rooms — the join flow and
 * the remembered rooms are the only sources of a name this build's wire offers.
 */
export function roomRowTitle(summary: ConversationSummary, room: RoomInfo | null): string {
  const name = room?.name ?? summary.title ?? 'Room';
  return `# ${name}`;
}

export function ConversationList(): ReactNode {
  const { accountId } = useMigo();
  const { items, loading, error, hasMore, loadMore, unread, lastPreviews } = useConversations();
  const rooms = useRooms();
  const openId = useOpenConversation();

  // Profiles resolve the sidebar's two name surfaces: the 1:1 peer (row title, avatar) and the
  // sender whose name prefixes a group's last-message preview.
  const profileIds = useMemo(() => {
    const ids: Id[] = [];
    const seen = new Set<Id>();
    const push = (id: Id | undefined): void => {
      if (id !== undefined && id !== accountId && !seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    };
    for (const item of items) {
      if (item.kind === ConversationKind.Direct) {
        push(item.members?.find((member) => member !== accountId));
      } else {
        // The preview's sender is cleartext on the sealed event too, so the prefix works even
        // before the body has been decrypted.
        push(lastPreviews.get(item.conversationId)?.senderId ?? item.lastMessage?.senderId);
      }
    }
    return ids;
  }, [items, lastPreviews, accountId]);
  const profiles = useProfiles(profileIds);

  if (items.length === 0 && loading) {
    return (
      <div className="center-fill">
        <Spinner />
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="center-fill">
        <div>
          <div className="emoji">
            <Icon name="chats" size={24} />
          </div>
          {error ?? 'No conversations yet. Start one with the + button.'}
        </div>
      </div>
    );
  }

  return (
    <div className="conversation-list">
      {items.map((item) => {
        const active = openId === item.conversationId;
        const room =
          item.kind === ConversationKind.Room ? rooms.infoFor(item.conversationId) : null;
        return (
          <Row
            key={item.conversationId}
            summary={item}
            peerName={peerNameFor(item, accountId, profiles, room)}
            peerAvatarUrl={peerAvatarFor(item, accountId, profiles)}
            subtitle={subtitleFor(item, accountId, profiles, lastPreviews, room)}
            active={active}
            unread={!active && (unread.has(item.conversationId) || item.lastSeq > item.readSeq)}
          />
        );
      })}
      {hasMore ? (
        <button
          type="button"
          className="btn btn-ghost btn-block"
          style={{ margin: 12 }}
          onClick={loadMore}
        >
          Load more
        </button>
      ) : null}
    </div>
  );
}

interface RowProps {
  summary: ConversationSummary;
  peerName: string;
  peerAvatarUrl: string | undefined;
  subtitle: string;
  active: boolean;
  unread: boolean;
}

function Row({ summary, peerName, peerAvatarUrl, subtitle, active, unread }: RowProps): ReactNode {
  const time = summary.lastMessage?.createdAt;
  return (
    <a
      href={conversationHref(summary.conversationId)}
      className={`conversation-row ${active ? 'active' : ''}`}
    >
      <Avatar name={peerName} id={summary.conversationId} size={44} avatarUrl={peerAvatarUrl} />
      <div className="conversation-main">
        <div className="conversation-name">{peerName}</div>
        <div className="conversation-sub">{subtitle}</div>
      </div>
      <div className="conversation-meta">
        {time ? <span className="conversation-time">{formatRelative(time)}</span> : <span />}
        {unread ? <span className="unread-dot" /> : null}
      </div>
    </a>
  );
}

/**
 * The row's second line: the last message when there is one, the membership subtitle otherwise.
 *
 * The preview falls back rather than blanks because a conversation with no `lastMessage` (a group
 * just created, or a room joined at its tip) still needs its subtitle to say what the row is. A
 * room's fallback prefers the room record's member count: the summary's member preview is capped
 * by the server and would call a thousand-member room a nine-member one.
 */
function subtitleFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: ReadonlyMap<Id, ResolvedProfile>,
  lastPreviews: ReadonlyMap<Id, IncomingMessage>,
  room: RoomInfo | null = null,
): string {
  const last = lastPreviews.get(summary.conversationId) ?? null;
  const senderId = last?.senderId ?? summary.lastMessage?.senderId;
  const senderLabel =
    senderId === undefined
      ? null
      : senderId === selfId
        ? 'You'
        : (profiles.get(senderId)?.displayName ?? null);
  return (
    lastMessagePreviewLine(summary, last?.content ?? null, senderLabel) ??
    (summary.kind === ConversationKind.Direct
      ? 'Direct message'
      : summary.kind === ConversationKind.Room
        ? `${room?.memberCount ?? summary.members?.length ?? 0} members`
        : `${summary.members?.length ?? 0} members`)
  );
}

function peerNameFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: ReadonlyMap<Id, ResolvedProfile>,
  room: RoomInfo | null = null,
): string {
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== selfId);
    return (other && profiles.get(other)?.displayName) || 'Direct message';
  }
  if (summary.kind === ConversationKind.Room) {
    return roomRowTitle(summary, room);
  }
  if (summary.title) {
    return summary.title;
  }
  return 'Group';
}

/**
 * The row's avatar image: a 1:1's peer through their resolved profile, any other conversation
 * through the summary's own avatar field (a room picture the server holds; that field is the
 * conversation's, not the profile's, and is unaffected by the profile avatar migration).
 */
function peerAvatarFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: ReadonlyMap<Id, ResolvedProfile>,
): string | undefined {
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== selfId);
    return other ? profiles.get(other)?.avatarUrl : undefined;
  }
  return summary.avatarUrl;
}
