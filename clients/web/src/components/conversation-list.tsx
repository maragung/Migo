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
  UserProfile,
} from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { messagePreview, truncate } from '@/lib/message-preview.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { conversationHref, useOpenConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
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

export function ConversationList(): ReactNode {
  const { accountId } = useMigo();
  const { items, loading, error, hasMore, loadMore, unread, lastPreviews } = useConversations();
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
          <div className="emoji">💬</div>
          {error ?? 'No conversations yet. Start one with the + button.'}
        </div>
      </div>
    );
  }

  return (
    <div className="conversation-list">
      {items.map((item) => {
        const active = openId === item.conversationId;
        return (
          <Row
            key={item.conversationId}
            summary={item}
            peerName={peerNameFor(item, accountId, profiles)}
            peerAvatarUrl={peerAvatarFor(item, accountId, profiles)}
            subtitle={subtitleFor(item, accountId, profiles, lastPreviews)}
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
 * just created, or a room joined at its tip) still needs its subtitle to say what the row is.
 */
function subtitleFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: ReadonlyMap<Id, UserProfile>,
  lastPreviews: ReadonlyMap<Id, IncomingMessage>,
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
      : `${summary.members?.length ?? 0} members`)
  );
}

function peerNameFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: Map<Id, UserProfile>,
): string {
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== selfId);
    return (other && profiles.get(other)?.displayName) || 'Direct message';
  }
  if (summary.title) {
    return summary.title;
  }
  return summary.kind === ConversationKind.Room ? 'Room' : 'Group';
}

function peerAvatarFor(
  summary: ConversationSummary,
  selfId: Id | null,
  profiles: Map<Id, UserProfile>,
): string | undefined {
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== selfId);
    return other ? profiles.get(other)?.avatarUrl : undefined;
  }
  return summary.avatarUrl;
}
