'use client';

/**
 * Shared conversation-list state for the authenticated shell.
 *
 * Mounted once under the chat layout so the sidebar and the open thread agree on one list, its unread
 * marks, and its ordering. Loading a page also primes each conversation's membership and subscribes to
 * its topic (that is what {@link MigoClient.loadConversations} does), so messages start arriving without
 * any extra step. Live inbound messages reorder the list and set an unread mark; opening a conversation
 * clears it. Nothing polls — reordering is driven by the SDK's message stream.
 *
 * The group lifecycle lands here too: a group's member events patch the summary's member list (and
 * rotate the sender-key chain, the crypto cost of membership churn), and its state deltas carry a
 * rename onto the row's title — see {@link applyMemberEvent} and {@link applyStateEvent}, the pure
 * projections the tests pin.
 *
 * The provider also owns the sidebar's last-message previews. A summary's `lastMessage` is a sealed
 * `MessageEvent`; the only way to read it is the SDK's decrypt-and-deliver path, so each page's
 * lastMessages are replayed through {@link MessagingDomain.ingest} and whatever opens is captured from
 * the same message stream the rest of the app listens to. An event that cannot open yet (its sender's
 * key distribution has not replayed on this fresh session) is buffered by the messaging layer and
 * surfaces through this same handler once it does — so the preview simply appears when it becomes
 * readable, with no second pipeline to keep in step.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { MemberChange } from '@migo/sdk';
import type {
  ConversationMemberEvent,
  ConversationStateEvent,
  ConversationSummary,
  Id,
  IncomingMessage,
} from '@migo/sdk';

import { useMigo } from './use-migo.js';

const PAGE_SIZE = 30;

/**
 * Applies one group member event onto the summary the list holds.
 *
 * Pure, so a test can pin it. The summary's `members` is the roster this device knows; a join adds
 * the account (a no-op when already listed, since the server also counts re-joins), and a departure
 * — a leave, a kick, or a ban — removes it. A group's own mute has nothing to do with the
 * summary-level `mutedUntil` (the caller's own row), so it is not touched here.
 */
export function applyMemberEvent(
  summary: ConversationSummary,
  event: ConversationMemberEvent,
): ConversationSummary {
  const held = summary.members;
  if (held === undefined) {
    return summary;
  }
  if (event.change === MemberChange.Joined) {
    return held.includes(event.userId)
      ? summary
      : { ...summary, members: [...held, event.userId] };
  }
  const departing =
    event.change === MemberChange.Left ||
    event.change === MemberChange.Kicked ||
    event.change === MemberChange.Banned;
  if (!departing || !held.includes(event.userId)) {
    return summary;
  }
  return { ...summary, members: held.filter((id) => id !== event.userId) };
}

/**
 * Applies one coalesced group-state delta onto the summary the list holds.
 *
 * Pure, so a test can pin it. The event is a delta by protocol: each field it carries replaces the
 * held value, and each field it omits leaves the held value alone — a rename writes the title, and
 * anything else changes nothing.
 */
export function applyStateEvent(
  summary: ConversationSummary,
  event: ConversationStateEvent,
): ConversationSummary {
  if (event.title === undefined || summary.title === event.title) {
    return summary;
  }
  return { ...summary, title: event.title };
}

export interface ConversationsContextValue {
  items: ConversationSummary[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  loadMore: () => void;
  reload: () => void;
  unread: ReadonlySet<Id>;
  markRead: (conversationId: Id) => void;
  /** Insert a just-created conversation at the top if it is not already present. */
  noteConversation: (summary: ConversationSummary) => void;
  /**
   * Drops a conversation from the shared list — the local echo of leaving a room, whose
   * conversation the server has closed for this account and the sidebar must stop offering.
   */
  forgetConversation: (conversationId: Id) => void;
  /**
   * The newest decrypted message per conversation, for the sidebar's preview line. Sparse by
   * design: a conversation whose last message has not opened (or was deleted) is simply absent.
   */
  lastPreviews: ReadonlyMap<Id, IncomingMessage>;
}

const ConversationsContext = createContext<ConversationsContextValue | null>(null);

export function ConversationsProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId, resetNonce } = useMigo();

  const [items, setItems] = useState<ConversationSummary[]>([]);
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unread, setUnread] = useState<ReadonlySet<Id>>(new Set());
  const [lastPreviews, setLastPreviews] = useState<ReadonlyMap<Id, IncomingMessage>>(new Map());

  const itemsRef = useRef<ConversationSummary[]>([]);
  const accountIdRef = useRef<Id | null>(accountId);
  itemsRef.current = items;
  accountIdRef.current = accountId;
  /** True while replaying summaries' lastMessages for preview, so the live handler can tell a replay from traffic. */
  const previewReplayRef = useRef(false);

  const load = useCallback(
    async (reset: boolean): Promise<void> => {
      if (!client) {
        return;
      }
      setLoading(true);
      setError(null);
      try {
        const response = await client.loadConversations(PAGE_SIZE, reset ? undefined : cursor);
        setItems((prev) =>
          reset ? response.conversations : mergeById(prev, response.conversations),
        );
        setCursor(response.nextCursor);
        // Replay each sealed lastMessage through the decrypt path to prime the preview map. Delivery
        // is synchronous and reaches every message listener; the flag keeps this provider's own
        // live-stream handling (reorder, unread) from firing for a replay, since the summary already
        // fixes the row's position and the unread badge is computed from its cursors.
        previewReplayRef.current = true;
        try {
          for (const summary of response.conversations) {
            if (summary.lastMessage !== undefined) {
              client.messaging.ingest(summary.lastMessage);
            }
          }
        } finally {
          previewReplayRef.current = false;
        }
      } catch {
        setError('Could not load conversations.');
      } finally {
        setLoading(false);
      }
    },
    [client, cursor],
  );

  const reload = useCallback((): void => {
    void load(true);
  }, [load]);

  const loadMore = useCallback((): void => {
    if (cursor && !loading) {
      void load(false);
    }
  }, [cursor, loading, load]);

  const markRead = useCallback((conversationId: Id): void => {
    setUnread((prev) => {
      if (!prev.has(conversationId)) {
        return prev;
      }
      const next = new Set(prev);
      next.delete(conversationId);
      return next;
    });
  }, []);

  const noteConversation = useCallback((summary: ConversationSummary): void => {
    setItems((prev) => {
      if (prev.some((item) => item.conversationId === summary.conversationId)) {
        return prev;
      }
      return [summary, ...prev];
    });
  }, []);

  const forgetConversation = useCallback((conversationId: Id): void => {
    setItems((prev) => prev.filter((item) => item.conversationId !== conversationId));
    setUnread((prev) => {
      if (!prev.has(conversationId)) {
        return prev;
      }
      const next = new Set(prev);
      next.delete(conversationId);
      return next;
    });
  }, []);

  // Initial load, and a full resync whenever the session resets (topics were re-subscribed by the SDK).
  useEffect(() => {
    if (!client) {
      return;
    }
    void load(true);
    // `load` closes over `cursor`, but a reset must always start from the top; depend on the session only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, resetNonce]);

  // Live reordering, unread marks, and preview updates from the message stream.
  useEffect(() => {
    if (!client) {
      return;
    }
    const off = client.messaging.onMessage((message) => {
      // Whatever opened is, by delivery order, the conversation's newest readable message — true
      // for live traffic and for the preview replay alike, so both record it here.
      setLastPreviews((prev) => {
        if (prev.get(message.conversationId)?.messageId === message.messageId) {
          return prev;
        }
        const next = new Map(prev);
        next.set(message.conversationId, message);
        return next;
      });
      if (previewReplayRef.current) {
        return;
      }
      const index = itemsRef.current.findIndex(
        (item) => item.conversationId === message.conversationId,
      );
      if (index === -1) {
        // A message for a conversation we have not listed yet (e.g. a peer started it): fetch the list.
        void load(true);
      } else if (index > 0) {
        setItems((prev) => {
          const found = prev.find((item) => item.conversationId === message.conversationId);
          if (!found) {
            return prev;
          }
          return [found, ...prev.filter((item) => item.conversationId !== message.conversationId)];
        });
      }
      if (message.senderId !== accountIdRef.current) {
        setUnread((prev) => {
          const next = new Set(prev);
          next.add(message.conversationId);
          return next;
        });
      }
    });

    // A deleted last message must not keep previewing its (now gone) content: drop the preview so
    // the row falls back to the summary's sealed event, which the server has already tombstoned.
    const offDeletion = client.messaging.onDeletion((deletion) => {
      setLastPreviews((prev) => {
        if (prev.get(deletion.conversationId)?.messageId !== deletion.messageId) {
          return prev;
        }
        const next = new Map(prev);
        next.delete(deletion.conversationId);
        return next;
      });
    });

    return () => {
      off();
      offDeletion();
    };
  }, [client, load]);

  // The live group streams: membership movement and coalesced metadata. A member event also rotates
  // the conversation's outbound sender-key chain — membership churn is a crypto event before it is a
  // UI one, and the next send re-distributes the fresh chain to whoever belongs now, so a removed
  // member cannot read what is sealed after their departure.
  useEffect(() => {
    if (!client) {
      return;
    }
    const offMember = client.conversations.onMember((event) => {
      if (!itemsRef.current.some((item) => item.conversationId === event.conversationId)) {
        return;
      }
      client.messaging.rotateSenderKey(event.conversationId);
      setItems((prev) =>
        prev.map((item) =>
          item.conversationId === event.conversationId
            ? applyMemberEvent(item, event)
            : item,
        ),
      );
    });
    const offState = client.conversations.onState((event) => {
      setItems((prev) =>
        prev.map((item) =>
          item.conversationId === event.conversationId ? applyStateEvent(item, event) : item,
        ),
      );
    });
    return () => {
      offMember();
      offState();
    };
  }, [client, resetNonce]);

  const value: ConversationsContextValue = {
    items,
    loading,
    error,
    hasMore: cursor !== undefined,
    loadMore,
    reload,
    unread,
    markRead,
    noteConversation,
    forgetConversation,
    lastPreviews,
  };

  return <ConversationsContext.Provider value={value}>{children}</ConversationsContext.Provider>;
}

export function useConversations(): ConversationsContextValue {
  const value = useContext(ConversationsContext);
  if (value === null) {
    throw new Error('useConversations must be used within a ConversationsProvider');
  }
  return value;
}

/** Appends only the summaries not already present, so pagination never duplicates a row. */
function mergeById(
  existing: ConversationSummary[],
  incoming: ConversationSummary[],
): ConversationSummary[] {
  const seen = new Set(existing.map((item) => item.conversationId));
  return [...existing, ...incoming.filter((item) => !seen.has(item.conversationId))];
}
