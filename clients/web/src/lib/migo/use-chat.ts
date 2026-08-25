'use client';

/**
 * The message thread for one conversation.
 *
 * The key ordering constraint: both historical and live messages surface through the same decrypted
 * stream. {@link MigoClient.catchUp} replays fetched history through the decryption path, which delivers
 * each message to the `onMessage` handler — the very same handler live delivery uses. So this hook
 * subscribes first, then catches up, and treats every message identically, de-duplicating by id and
 * keeping the list ordered by sequence number. Sending optimistically echoes the sent message locally,
 * because the server's fan-out excludes our own sending device.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import { ContentType, ReceiptKind, TypingState } from '@migo/sdk';
import type { Id, IncomingMessage, TextContent, TypingEvent } from '@migo/sdk';

import { useMigo } from './use-migo.js';

/** How many pages of history to replay at most, so a very long conversation stays bounded. */
const MAX_CATCHUP_PAGES = 5;
const CATCHUP_PAGE = 200;
/** Clear a peer's typing indicator this long after the last Start, in case a Stop is missed. */
const TYPING_TIMEOUT_MS = 4000;

export interface ChatThread {
  messages: IncomingMessage[];
  loading: boolean;
  error: string | null;
  typingUser: Id | null;
  send: (text: string) => Promise<void>;
  setTyping: (isTyping: boolean) => void;
}

export function useChat(conversationId: Id): ChatThread {
  const { client, accountId, resetNonce } = useMigo();

  const [messages, setMessages] = useState<IncomingMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [typingUser, setTypingUser] = useState<Id | null>(null);

  const typingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastReadSeqRef = useRef(0);

  const upsert = useCallback((incoming: IncomingMessage): void => {
    setMessages((prev) => {
      if (prev.some((message) => message.messageId === incoming.messageId)) {
        return prev;
      }
      const next = [...prev, incoming];
      next.sort((a, b) => a.seq - b.seq);
      return next;
    });
  }, []);

  // Subscribe to the decrypted stream and typing, then replay history through the same path.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    setMessages([]);
    setLoading(true);
    setError(null);
    setTypingUser(null);
    lastReadSeqRef.current = 0;

    const offMessage = client.messaging.onMessage((message) => {
      if (message.conversationId !== conversationId) {
        return;
      }
      upsert(message);
      // Acknowledge receipt of a peer's message so they see a read marker.
      if (message.senderId !== accountId && message.seq > lastReadSeqRef.current) {
        lastReadSeqRef.current = message.seq;
        void client.messaging
          .sendReceipt(conversationId, ReceiptKind.Read, message.seq)
          .catch(() => {});
      }
    });

    const offTyping = client.typing.onTyping((event: TypingEvent) => {
      if (
        event.conversationId !== conversationId ||
        event.userId === undefined ||
        event.userId === accountId
      ) {
        return;
      }
      if (event.state === TypingState.Start) {
        setTypingUser(event.userId);
        if (typingTimerRef.current) {
          clearTimeout(typingTimerRef.current);
        }
        typingTimerRef.current = setTimeout(() => setTypingUser(null), TYPING_TIMEOUT_MS);
      } else {
        setTypingUser(null);
      }
    });

    async function catchUp(): Promise<void> {
      try {
        await client!.watchConversation(conversationId);
        let haveSeq = 0;
        for (let page = 0; page < MAX_CATCHUP_PAGES; page += 1) {
          const response = await client!.catchUp(conversationId, haveSeq, CATCHUP_PAGE);
          haveSeq = response.toSeq;
          if (!response.more) {
            break;
          }
        }
      } catch {
        if (!cancelled) {
          setError('Could not load this conversation.');
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }
    void catchUp();

    return () => {
      cancelled = true;
      offMessage();
      offTyping();
      if (typingTimerRef.current) {
        clearTimeout(typingTimerRef.current);
        typingTimerRef.current = null;
      }
    };
  }, [client, conversationId, accountId, resetNonce, upsert]);

  const send = useCallback(
    async (text: string): Promise<void> => {
      const trimmed = text.trim();
      if (!client || !accountId || trimmed.length === 0) {
        return;
      }
      const content: TextContent = { type: ContentType.Text, text: trimmed };
      const accepted = await client.messaging.send(conversationId, content);
      // Optimistic local echo: the sender is excluded from the server's fan-out.
      upsert({
        messageId: accepted.messageId,
        conversationId,
        seq: accepted.seq,
        senderId: accountId,
        senderDevice: client.deviceId,
        content,
        createdAt: accepted.createdAt,
      });
      void client.typing.setTyping(conversationId, TypingState.Stop).catch(() => {});
    },
    [client, accountId, conversationId, upsert],
  );

  const setTyping = useCallback(
    (isTyping: boolean): void => {
      if (!client) {
        return;
      }
      void client.typing
        .setTyping(conversationId, isTyping ? TypingState.Start : TypingState.Stop)
        .catch(() => {});
    },
    [client, conversationId],
  );

  return { messages, loading, error, typingUser, send, setTyping };
}
