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
import type { Id, IncomingMessage, MigoClient, TextContent, TypingEvent } from '@migo/sdk';

import { useMigo } from './use-migo.js';
import { uploadDocumentAttachment, uploadImageAttachment } from './media.js';
import { sealReaction, sealTextEdit } from './seal.js';
import { uploadVoiceNote } from './voice.js';
import type { VoiceRecording } from './voice.js';

/** How many pages of history to replay at most, so a very long conversation stays bounded. */
const MAX_CATCHUP_PAGES = 5;
const CATCHUP_PAGE = 200;
/** Clear a peer's typing indicator this long after the last Start, in case a Stop is missed. */
const TYPING_TIMEOUT_MS = 4000;
/** How often the thread drops messages whose sealed lifetime has passed. */
const EXPIRY_SWEEP_MS = 1000;

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

/**
 * Whether a message's sealed lifetime has passed. The lifetime travels inside the message's own
 * ciphertext (the wire's `expires_in_ms` reaches only the server), so the deadline this reads is
 * the one every receiver holds — and the countdown runs on the receiving clock by design: the
 * sweep's own doc says a client must not wait for the server to tell it a message is gone.
 */
export function messageExpired(message: ThreadMessage, now = Date.now()): boolean {
  const lifetime = contentLifetime(message.content);
  return lifetime !== undefined && message.createdAt + lifetime <= now;
}

/** The sealed disappearing lifetime a content body carries, whatever its type. */
function contentLifetime(content: ThreadMessage['content']): number | undefined {
  return content.type === ContentType.Text ||
    content.type === ContentType.MediaRef ||
    content.type === ContentType.VoiceNoteRef
    ? content.expiresInMs
    : undefined;
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
  send: (text: string, expiresInMs?: number) => Promise<void>;
  /** Uploads a picked image file and sends the message that references it. */
  sendAttachment: (file: File) => Promise<void>;
  /** Uploads a finished voice note recording and sends the message that references it. */
  sendVoiceNote: (recording: VoiceRecording) => Promise<void>;
  /**
   * The disappearing-message lifetime the composer has armed, in milliseconds — the value the
   * next send seals beside its content. Null when the next message is a normal, kept one.
   */
  expiresAfterMs: number | null;
  /** Arms or clears the disappearing mode the next send carries. */
  setExpiresAfterMs: (ms: number | null) => void;
  setTyping: (isTyping: boolean) => void;
  /** True while the deletion request for a message is still in flight. */
  deleting: boolean;
  deleteMessage: (messageId: Id) => void;
  /**
   * Replaces one of our own text messages' content: re-seals the replacement text exactly as a
   * send would and hands the envelope to `editMessage`, which keeps the message's id and seq.
   */
  editMessage: (messageId: Id, text: string) => void;
  /**
   * Sets one of the quick reactions on a message: the emoji is sealed before it rides the wire,
   * so the server learns only that *some* reaction was set, never which.
   */
  react: (messageId: Id, emoji: string) => void;
  /**
   * Whether the thread holds less than its full history: the initial replay is page-bounded, so a
   * long conversation can be cut short. `loadEarlier` is what reaches the rest.
   */
  hasEarlier: boolean;
  /** True while a page of earlier history is being fetched. */
  loadingEarlier: boolean;
  loadEarlier: () => void;
}

/**
 * What the thread needs to know about where its media is going.
 *
 * `endToEnd` decides whether attachments and voice notes are sealed before upload — see
 * `lib/migo/media.js`'s module doc for the rule and its one exception. Defaults to true (every
 * direct conversation and group is end-to-end), so a caller that knows nothing sends sealed
 * media, and the chat window passes the conversation summary's honest answer.
 */
export interface ChatCryptoOptions {
  endToEnd?: boolean;
}

export function useChat(conversationId: Id, options: ChatCryptoOptions = {}): ChatThread {
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
  // The disappearing mode the composer has armed: the lifetime the next send seals beside its
  // content. It stays armed across sends (the mode is a setting, not a one-shot), the way the
  // reply target is a one-shot it is not.
  const [expiresAfterMs, setExpiresAfterMs] = useState<number | null>(null);

  const typingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastReadSeqRef = useRef(0);
  /**
   * The live message list for async reads (paging history must not race a state update), and the
   * cursor the next backwards page continues from.
   */
  const messagesRef = useRef<ThreadMessage[]>([]);
  messagesRef.current = messages;
  const earlierCursorRef = useRef(0);
  /**
   * The thread this hook's current state belongs to, by client and conversation identity.
   *
   * A session reset re-runs the effect on the same thread; comparing against this tells that run
   * from a genuine thread switch, which is the difference between "sync the gap" and "start over".
   */
  const threadRef = useRef<{ client: MigoClient | null; conversationId: Id }>({
    client: null,
    conversationId: conversationId,
  });

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
  //
  // The effect re-runs for two reasons and treats them differently: a new thread (a different
  // conversation or client) starts from an empty transcript, while a session reset on the same
  // thread keeps everything already on screen and syncs *only the gap* — section 158 forbids the
  // full resync that wiping here would force, because a reconnect that dropped and refetched a
  // whole held conversation is indistinguishable, on the wire, from a client that had nothing.
  useEffect(() => {
    if (!client) {
      return;
    }
    // The same client instance survives a reconnect (the transport reconnects in place), so
    // identity here is "the same thread under the same session", which is exactly the case where
    // the transcript stays.
    const sameThread =
      threadRef.current.client === client && threadRef.current.conversationId === conversationId;
    threadRef.current = { client, conversationId };
    let cancelled = false;
    if (!sameThread) {
      setMessages([]);
      setLoading(true);
      setError(null);
      setReadUpTo(0);
      setHasEarlier(false);
      lastReadSeqRef.current = 0;
      earlierCursorRef.current = 0;
    }
    // Typing is a live signal, not history: a reset invalidates whatever indicator was on
    // screen, because the typing that produced it predates the new session's stream.
    setTypingUser(null);

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
        // A fresh thread replays from the beginning. A reset on a held thread starts from the
        // watermark — the highest sequence the messaging domain ingested contiguously — so the
        // fetch asks for exactly the messages the outage cost, which is section 158's "sync only
        // the gap, never a full resync".
        const held = sameThread ? client!.messaging.watermark(conversationId) : undefined;
        let haveSeq = held !== undefined && messagesRef.current.length > 0 ? held : 0;
        let replayedAll = false;
        for (let page = 0; page < MAX_CATCHUP_PAGES; page += 1) {
          // A hidden page parks here: the fetch in flight finishes (awaited above), the next one
          // does not start until the page is visible again — stopped neatly, not abandoned.
          await client!.whenVisible();
          if (cancelled) {
            return;
          }
          const response = await client!.catchUp(conversationId, haveSeq, CATCHUP_PAGE);
          haveSeq = response.toSeq;
          if (!response.more) {
            replayedAll = true;
            break;
          }
        }
        if (!cancelled) {
          if (sameThread) {
            // The transcript already carries its own "is there older history" answer from the
            // pages the user has loaded; only a budget-stopped walk can add the *newer* history
            // marker, never clear what the walk below already established.
            if (!replayedAll) {
              setHasEarlier(true);
            }
          } else {
            // The replay pages forward from the thread's first sequence, so only the page budget —
            // never the server — can stop it short. A short replay means history above the held range
            // exists; "Load earlier" is what reaches it, paging down from the newest.
            setHasEarlier(!replayedAll);
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
   * The local half of a disappearing message: the drop when a sealed lifetime passes.
   *
   * The server sweeps its own store on a one-minute tick, but the deadline is the client's to
   * honour — the sweeper publishes nothing, so a client that waited for the server would show a
   * message for a full minute past the moment it promised to vanish. Each message's lifetime is
   * sealed inside its own ciphertext (the wire never echoes `expires_in_ms` back), so this is the
   * only place the deadline can be read. The sweep is one `setMessages` per tick that actually
   * drops something; a quiet thread costs one `some()` scan per second.
   *
   * The row is removed outright, not tombstoned: unlike a deletion — which names a message that
   * may still be unread, so its place in the transcript is kept — an expiry is the message saying
   * it never wanted to be remembered. The seq numbering gains the same gap a hard purge leaves,
   * and the sync path's `Truncated` status already treats a gap as honest.
   */
  useEffect(() => {
    if (messages.length === 0) {
      return;
    }
    const timer = setInterval(() => {
      setMessages((prev) => {
        const kept = prev.filter((message) => !messageExpired(message));
        return kept.length === prev.length ? prev : kept;
      });
    }, EXPIRY_SWEEP_MS);
    return () => clearInterval(timer);
  }, [messages.length]);

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
    async (text: string, expiresInMs?: number): Promise<void> => {
      const trimmed = text.trim();
      if (!client || !accountId || trimmed.length === 0) {
        return;
      }
      // The lifetime is sealed inside the content (where every receiver reads it) and stated on
      // the send (where the server computes its own expiry from its clock). One argument serves
      // both because they are the same number, chosen once by the sender.
      const lifetime = expiresInMs ?? expiresAfterMs ?? undefined;
      const content: TextContent = {
        type: ContentType.Text,
        text: trimmed,
        ...(lifetime !== undefined ? { expiresInMs: lifetime } : {}),
      };
      // A reply carries the target's id as a threading hint the server stores and replays; the
      // composer's preview state, not the message content, is what makes it a reply in the UI.
      const sendOptions = {
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
        ...(lifetime !== undefined ? { expiresInMs: lifetime } : {}),
      };
      const accepted = await client.messaging.send(conversationId, content, sendOptions);
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
    [client, accountId, conversationId, upsert, replyTo, expiresAfterMs],
  );

  /**
   * Uploads a picked file and sends the media message that references it.
   *
   * An image goes down the image path (which keeps the room-plaintext branch, because an image may
   * legitimately be sent into a server-readable room). Anything else is a document, which rides the
   * document path — sealed only, matching the composer's gating of the attach button to end-to-end
   * conversations. The upload happens before any message is sent, so a failed upload rejects here
   * without the conversation ever seeing a dangling reference. A reply target in flight applies to
   * the media message exactly as it would to a text one.
   */
  const sendAttachment = useCallback(
    async (file: File): Promise<void> => {
      if (!client || !accountId) {
        return;
      }
      const content = file.type.startsWith('image/')
        ? await uploadImageAttachment(client, conversationId, file, options)
        : await uploadDocumentAttachment(client, conversationId, file);
      // A message the composer armed as disappearing carries its lifetime whatever its body —
      // text, image, document — because the promise "this vanishes" is about the send, not the
      // medium. The lifetime rides both the content and the wire field, as in `send`.
      const lifetime = expiresAfterMs ?? undefined;
      if (lifetime !== undefined) {
        content.expiresInMs = lifetime;
      }
      const sendOptions = {
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
        ...(lifetime !== undefined ? { expiresInMs: lifetime } : {}),
      };
      const accepted = await client.messaging.send(conversationId, content, sendOptions);
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
    [client, accountId, conversationId, options, upsert, replyTo, expiresAfterMs],
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
      const content = await uploadVoiceNote(client, conversationId, recording, options);
      // The same armed-lifetime rule as an attachment: a disappearing voice note disappears too.
      const lifetime = expiresAfterMs ?? undefined;
      if (lifetime !== undefined) {
        content.expiresInMs = lifetime;
      }
      const sendOptions = {
        ...(replyTo ? { replyTo: replyTo.messageId } : {}),
        ...(lifetime !== undefined ? { expiresInMs: lifetime } : {}),
      };
      const accepted = await client.messaging.send(conversationId, content, sendOptions);
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
    [client, accountId, conversationId, options, upsert, replyTo, expiresAfterMs],
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

  /**
   * Edits one of our own text messages in place. The replacement text is sealed through the same
   * group-crypto layer a send uses (see lib/migo/seal.ts) and the server stores the new envelope
   * under the existing id; on success the local copy is updated to the new text and stamped
   * edited, because the edit echo comes back through the stream like any other redelivery.
   */
  const editMessage = useCallback(
    (messageId: Id, text: string): void => {
      const trimmed = text.trim();
      if (!client || trimmed.length === 0) {
        return;
      }
      const envelope = sealTextEdit(client, conversationId, trimmed);
      client.messaging
        .editMessage(conversationId, messageId, envelope)
        .then(() => {
          setMessages((prev) =>
            prev.map((message) =>
              message.messageId === messageId && message.content.type === ContentType.Text
                ? {
                    ...message,
                    content: { type: ContentType.Text, text: trimmed },
                    editedAt: Date.now(),
                  }
                : message,
            ),
          );
        })
        .catch(() => {
          // A refused edit leaves the original message exactly as it was.
        });
    },
    [client, conversationId],
  );

  /**
   * Reacts to a message. The emoji is sealed before it is sent, and the send is fire-and-forget
   * in the UI: the reaction surfaces through the thread's stream when the server broadcasts it,
   * and a failure costs nothing to leave unshown — the control stays for a retry.
   */
  const react = useCallback(
    (messageId: Id, emoji: string): void => {
      if (!client) {
        return;
      }
      const envelope = sealReaction(client, conversationId, messageId, emoji);
      client.messaging.sendReaction(messageId, conversationId, envelope).catch(() => {});
    },
    [client, conversationId],
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
    expiresAfterMs,
    setExpiresAfterMs,
    setTyping,
    deleting,
    deleteMessage,
    editMessage,
    react,
    hasEarlier,
    loadingEarlier,
    loadEarlier,
  };
}
