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
 *
 * Deletions and read receipts arrive as separate stream events and are folded into the same state: a
 * deletion turns the message it names into a tombstone (the row keeps its sequence, so the thread
 * never develops a hole a sync would misread as lost data), and a peer's Read receipt advances a
 * watermark that the message list renders as a two-tick read marker on our own messages.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import { ContentType, ReceiptKind, TypingState } from '@migo/sdk';
import type { Id, IncomingMessage, TextContent, TypingEvent } from '@migo/sdk';

import { useMigo } from './use-migo.js';
import { uploadImageAttachment } from './media.js';
import { uploadVoiceNote } from './voice.js';
import type { VoiceRecording } from './voice.js';

/** How many pages of history to replay at most, so a very long conversation stays bounded. */
const MAX_CATCHUP_PAGES = 5;
const CATCHUP_PAGE = 200;
/** Clear a peer's typing indicator this long after the last Start, in case a Stop is missed. */
const TYPING_TIMEOUT_MS = 4000;

/**
 * A thread message plus the tombstone mark the deletion stream sets.
 *
 * `deleted` is UI-only state derived from `onDeletion`; keeping it on the message (rather than a
 * separate id set) means the list stays a single ordered array and a tombstone cannot drift out of
 * the position the deleted message occupied.
 */
export interface ThreadMessage extends IncomingMessage {
  deleted?: boolean;
}

export interface ChatThread {
  messages: ThreadMessage[];
  loading: boolean;
  error: string | null;
  typingUser: Id | null;
  /**
   * The newest Read receipt from another member: our messages at or below this seq have been read.
   * In a 1:1 that member is the peer; in a group it is whoever acknowledged latest, so the marker
   * reads as "read" once anyone has.
   */
  readUpTo: number;
  /** The decrypted preview target the composer quotes when replying, or null when not replying. */
  replyTo: ThreadMessage | null;
  /** Marks a message as the reply target (and clears it again when passed null). */
  setReplyTo: (message: ThreadMessage | null) => void;
  send: (text: string) => Promise<void>;
  /** Uploads a picked image file and sends the message that references it. */
  sendAttachment: (file: File) => Promise<void>;
  /** Uploads a finished voice note recording and sends the message that references it. */
  sendVoiceNote: (recording: VoiceRecording) => Promise<void>;
  setTyping: (isTyping: boolean) => void;
  /** True while the deletion request for a message is still in flight. */
  deleting: boolean;
  deleteMessage: (messageId: Id) => void;
  /**
   * Whether the thread holds less than its full history: the initial replay is page-bounded, so a
   * long conversation can be cut short. `loadEarlier` is what reaches the rest.
   */
  hasEarlier: boolean;
  /** True while a page of earlier history is being fetched. */
  loadingEarlier: boolean;
  loadEarlier: () => void;
}

export function useChat(conversationId: Id): ChatThread {
  const { client, accountId, resetNonce } = useMigo();

  const [messages, setMessages] = useState<ThreadMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [typingUser, setTypingUser] = useState<Id | null>(null);
  const [readUpTo, setReadUpTo] = useState(0);
  const [replyTo, setReplyTo] = useState<ThreadMessage | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [hasEarlier, setHasEarlier] = useState(false);
  const [loadingEarlier, setLoadingEarlier] = useState(false);

  const typingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastReadSeqRef = useRef(0);
  /**
   * The live message list for async reads (paging history must not race a state update), and the
   * cursor the next backwards page continues from.
   */
  const messagesRef = useRef<ThreadMessage[]>([]);
  messagesRef.current = messages;
  const earlierCursorRef = useRef(0);

  const upsert = useCallback((incoming: IncomingMessage): void => {
    setMessages((prev) => {
      if (prev.some((message) => message.messageId === incoming.messageId)) {
        return prev;
      }
      const next: ThreadMessage[] = [...prev, incoming];
      next.sort((a, b) => a.seq - b.seq);
      return next;
    });
  }, []);

  /**
   * Turns a deletion into a tombstone, inserting one for a message we never held.
   *
   * Catch-up replays history in which the tombstone replaces the original event, so a deletion can
   * name a message this device never decrypted. Inserting a placeholder keeps the row (and the
   * sequence numbering it carries) visible, which is exactly what a converging transcript wants.
   */
  const markDeleted = useCallback((messageId: Id, stub: ThreadMessage): void => {
    setMessages((prev) => {
      const existing = prev.find((message) => message.messageId === messageId);
      if (existing === undefined) {
        const next = [...prev, stub];
        next.sort((a, b) => a.seq - b.seq);
        return next;
      }
      if (existing.deleted) {
        return prev;
      }
      return prev.map((message) =>
        message.messageId === messageId ? { ...message, deleted: true } : message,
      );
    });
  }, []);

  // Subscribe to the decrypted stream, typing, deletions, and receipts, then replay history through
  // the same path.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    setMessages([]);
    setLoading(true);
    setError(null);
    setTypingUser(null);
    setReadUpTo(0);
    setHasEarlier(false);
    lastReadSeqRef.current = 0;
    earlierCursorRef.current = 0;

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

    const offDeletion = client.messaging.onDeletion((deletion) => {
      if (deletion.conversationId !== conversationId) {
        return;
      }
      markDeleted(deletion.messageId, {
        messageId: deletion.messageId,
        conversationId,
        seq: deletion.seq,
        senderId: deletion.senderId,
        senderDevice: deletion.senderDevice,
        content: { type: ContentType.Text, text: '' },
        createdAt: deletion.createdAt,
        deleted: true,
      });
    });

    const offReceipt = client.messaging.onReceipt((receipt) => {
      // The watermark is a floor, not an assignment: a receipt racing the thread switch or an
      // out-of-order redelivery must never move the marker backwards. Our own receipts (echoed to
      // this device by the server's fan-out) are excluded so we never mark our messages read to
      // ourselves; the server stamps the reading account on every receipt it broadcasts.
      if (
        receipt.conversationId !== conversationId ||
        receipt.kind !== ReceiptKind.Read ||
        receipt.userId === undefined ||
        receipt.userId === accountId
      ) {
        return;
      }
      setReadUpTo((prev) => Math.max(prev, receipt.seq));
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
        let replayedAll = false;
        for (let page = 0; page < MAX_CATCHUP_PAGES; page += 1) {
          const response = await client!.catchUp(conversationId, haveSeq, CATCHUP_PAGE);
          haveSeq = response.toSeq;
          if (!response.more) {
            replayedAll = true;
            break;
          }
        }
        if (!cancelled) {
          // The replay pages forward from the thread's first sequence, so only the page budget —
          // never the server — can stop it short. A short replay means history above the held range
          // exists; "Load earlier" is what reaches it, paging down from the newest.
          setHasEarlier(!replayedAll);
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
      offDeletion();
      offReceipt();
      offTyping();
      if (typingTimerRef.current) {
        clearTimeout(typingTimerRef.current);
        typingTimerRef.current = null;
      }
    };
  }, [client, conversationId, accountId, resetNonce, upsert, markDeleted]);

  /**
   * Pages unreplayed history into the thread, one page per click.
   *
   * {@link MigoClient.catchUp} fetches forward only, so the backwards page is fetched through the
   * sync domain directly and each event replayed through `ingest` — the same decrypt-and-deliver
   * path catchUp uses, which is what keeps historical key distributions working when a page of
   * content arrives before the key that unlocks it (the messaging layer buffers and drains it).
   *
   * The first page is fetched from the newest (`haveSeq` 0), because the forward replay starts at
   * the thread's first sequence and can only ever be short at the tip; the cursor then continues
   * downward from each page's `fromSeq`. Ingested duplicates are dropped by `upsert`'s id check, so
   * the walk naturally terminates against history already held.
   */
  const loadEarlier = useCallback((): void => {
    if (!client || loadingEarlier) {
      return;
    }
    setLoadingEarlier(true);
    async function fetch(): Promise<void> {
      try {
        const response = await client!.sync.fetch(
          conversationId,
          earlierCursorRef.current,
          CATCHUP_PAGE,
          { backwards: true },
        );
        for (const event of response.messages) {
          client!.messaging.ingest(event);
        }
        earlierCursorRef.current = response.fromSeq;
        // "more" is conservative (a full page may be the last one), and a page that is entirely
        // duplicates means the walk has reached history already held. Either signal retires the
        // button; anything else means older pages remain.
        const held = new Set(messagesRef.current.map((message) => message.messageId));
        const allKnown =
          response.messages.length > 0 &&
          response.messages.every((event) => held.has(event.messageId));
        setHasEarlier(response.more && !allKnown);
      } catch {
        // Leave the button in place: a failed page is retriable, and hiding it would present the
        // gap it was hiding as a complete transcript.
      } finally {
        setLoadingEarlier(false);
      }
    }
    void fetch();
  }, [client, conversationId, loadingEarlier]);

  const send = useCallback(
    async (text: string): Promise<void> => {
      const trimmed = text.trim();
      if (!client || !accountId || trimmed.length === 0) {
        return;
      }
      const content: TextContent = { type: ContentType.Text, text: trimmed };
      // A reply carries the target's id as a threading hint the server stores and replays; the
      // composer's preview state, not the message content, is what makes it a reply in the UI.
      const options = replyTo ? { replyTo: replyTo.messageId } : {};
      const accepted = await client.messaging.send(conversationId, content, options);
      // Optimistic local echo: the sender is excluded from the server's fan-out.
      upsert({
        messageId: accepted.messageId,
        conversationId,
        seq: accepted.seq,
        senderId: accountId,
        senderDevice: client.deviceId,
        content,
        createdAt: accepted.createdAt,
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
      });
      setReplyTo(null);
      void client.typing.setTyping(conversationId, TypingState.Stop).catch(() => {});
    },
    [client, accountId, conversationId, upsert, replyTo],
  );

  /**
   * Uploads a picked image file and sends the media message that references it.
   *
   * The upload happens before any message is sent, so a failed upload rejects here without the
   * conversation ever seeing a dangling reference. A reply target in flight applies to the media
   * message exactly as it would to a text one.
   */
  const sendAttachment = useCallback(
    async (file: File): Promise<void> => {
      if (!client || !accountId) {
        return;
      }
      const content = await uploadImageAttachment(client, conversationId, file);
      const options = replyTo ? { replyTo: replyTo.messageId } : {};
      const accepted = await client.messaging.send(conversationId, content, options);
      upsert({
        messageId: accepted.messageId,
        conversationId,
        seq: accepted.seq,
        senderId: accountId,
        senderDevice: client.deviceId,
        content,
        createdAt: accepted.createdAt,
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
      });
      setReplyTo(null);
      void client.typing.setTyping(conversationId, TypingState.Stop).catch(() => {});
    },
    [client, accountId, conversationId, upsert, replyTo],
  );

  /**
   * Uploads a finished voice note recording and sends the voice message that references it.
   *
   * The same ordering rule as {@link sendAttachment}: the upload completes before any message is
   * sent, so a failed upload rejects here without the conversation ever seeing a dangling
   * reference — and the cap the recorder already enforced is checked again at the upload itself.
   */
  const sendVoiceNote = useCallback(
    async (recording: VoiceRecording): Promise<void> => {
      if (!client || !accountId) {
        return;
      }
      const content = await uploadVoiceNote(client, conversationId, recording);
      const options = replyTo ? { replyTo: replyTo.messageId } : {};
      const accepted = await client.messaging.send(conversationId, content, options);
      upsert({
        messageId: accepted.messageId,
        conversationId,
        seq: accepted.seq,
        senderId: accountId,
        senderDevice: client.deviceId,
        content,
        createdAt: accepted.createdAt,
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
      });
      setReplyTo(null);
      void client.typing.setTyping(conversationId, TypingState.Stop).catch(() => {});
    },
    [client, accountId, conversationId, upsert, replyTo],
  );

  /**
   * Delete-for-everyone. The server only permits the sender to unsend, which is why the control is
   * only ever rendered on our own messages; a failure keeps the message (and its content) as-is.
   */
  const deleteMessage = useCallback(
    (messageId: Id): void => {
      if (!client || deleting) {
        return;
      }
      setDeleting(true);
      client.messaging
        .deleteMessage(conversationId, messageId, true)
        .catch(() => {})
        .finally(() => setDeleting(false));
    },
    [client, conversationId, deleting],
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

  return {
    messages,
    loading,
    error,
    typingUser,
    readUpTo,
    replyTo,
    setReplyTo,
    send,
    sendAttachment,
    sendVoiceNote,
    setTyping,
    deleting,
    deleteMessage,
    hasEarlier,
    loadingEarlier,
    loadEarlier,
  };
}
