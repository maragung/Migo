'use client';

import { useMemo } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id, UserProfile } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { conversationHref, useOpenConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

export function ConversationList(): ReactNode {
  const { accountId } = useMigo();
  const { items, loading, error, hasMore, loadMore, unread } = useConversations();
  const openId = useOpenConversation();

  const peerIds = useMemo(() => {
    const ids: Id[] = [];
    for (const item of items) {
      if (item.kind === ConversationKind.Direct) {
        const other = item.members?.find((member) => member !== accountId);
        if (other) {
          ids.push(other);
        }
      }
    }
    return ids;
  }, [items, accountId]);
  const profiles = useProfiles(peerIds);

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
  active: boolean;
  unread: boolean;
}

function Row({ summary, peerName, peerAvatarUrl, active, unread }: RowProps): ReactNode {
  const time = summary.lastMessage?.createdAt;
  return (
    <a
      href={conversationHref(summary.conversationId)}
      className={`conversation-row ${active ? 'active' : ''}`}
    >
      <Avatar name={peerName} id={summary.conversationId} size={44} avatarUrl={peerAvatarUrl} />
      <div className="conversation-main">
        <div className="conversation-name">{peerName}</div>
        <div className="conversation-sub">
          {summary.kind === ConversationKind.Direct
            ? 'Direct message'
            : `${summary.members?.length ?? 0} members`}
        </div>
      </div>
      <div className="conversation-meta">
        {time ? <span className="conversation-time">{formatRelative(time)}</span> : <span />}
        {unread ? <span className="unread-dot" /> : null}
      </div>
    </a>
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
