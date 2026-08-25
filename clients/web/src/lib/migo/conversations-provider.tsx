'use client';

/**
 * Shared conversation-list state for the authenticated shell.
 *
 * Mounted once under the chat layout so the sidebar and the open thread agree on one list, its unread
 * marks, and its ordering. Loading a page also primes each conversation's membership and subscribes to
 * its topic (that is what {@link MigoClient.loadConversations} does), so messages start arriving without
 * any extra step. Live inbound messages reorder the list and set an unread mark; opening a conversation
 * clears it. Nothing polls — reordering is driven by the SDK's message stream.
 */

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { ConversationSummary, Id } from '@migo/sdk';

import { useMigo } from './use-migo.js';

const PAGE_SIZE = 30;

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
}

const ConversationsContext = createContext<ConversationsContextValue | null>(null);

export function ConversationsProvider({ children }: { children: ReactNode }): ReactNode {
  const { client, accountId, resetNonce } = useMigo();

  const [items, setItems] = useState<ConversationSummary[]>([]);
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unread, setUnread] = useState<ReadonlySet<Id>>(new Set());

  const itemsRef = useRef<ConversationSummary[]>([]);
  const accountIdRef = useRef<Id | null>(accountId);
  itemsRef.current = items;
  accountIdRef.current = accountId;

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

  // Initial load, and a full resync whenever the session resets (topics were re-subscribed by the SDK).
  useEffect(() => {
    if (!client) {
      return;
    }
    void load(true);
    // `load` closes over `cursor`, but a reset must always start from the top; depend on the session only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, resetNonce]);

  // Live reordering and unread marks from the message stream.
  useEffect(() => {
    if (!client) {
      return;
    }
    const off = client.messaging.onMessage((message) => {
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
    return off;
  }, [client, load]);

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
