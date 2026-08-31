'use client';

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent, ReactNode } from 'react';

import { ContentType } from '@migo/sdk';
import type { Id, IncomingMessage, MediaRefContent, MessageContent, UserProfile } from '@migo/sdk';

import { formatClock, formatDayLabel } from '@/lib/format.js';
import { messagePreview } from '@/lib/message-preview.js';
import type { ThreadMessage } from '@/lib/migo/use-chat.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';
import { TokenText } from './token-reference.js';
import { VoiceNoteBubble } from './voice-player.js';

/** How much of a quoted message the reply snippet above a bubble shows. */
const QUOTE_CHARS = 60;

/**
 * The cheap pre-test a message's text must pass before the token splitter runs — one regex test
 * instead of a split on every message, so threads without a $MIG mention pay nothing.
 */
const TICKER_EARLY = /\$mig\b/i;

/** The quick reactions the hover bar offers, in order. */
export const QUICK_REACTIONS: readonly string[] = ['👍', '❤️', '😂'];

/**
 * Resolves a media object to a short-lived URL the list may embed, or `null` when the object has
 * none. Supplied by the chat window from the live client; absent in a context with no client, where
 * media falls back to its text placeholder.
 */
export type MediaUrlResolver = (mediaId: Id) => Promise<string | null>;

/** The text a media message shows while (or instead of) its image: its caption, or a generic label. */
export function mediaLabel(content: MediaRefContent): string {
  return content.caption?.trim() || 'Attachment';
}

/**
 * One media reference, resolved to a URL and rendered as an image.
 *
 * The URL arrives asynchronously, so the first render is always the text placeholder and a
 * spinner — the list never blocks on the network. The sender's claimed `mimeType` is never acted
 * on: the frame embeds what the *server* serves at the URL, inside an `<img>`, where neither HTML
 * nor SVG scripts can execute, and the claim itself is never printed. Clicking opens a lightbox
 * overlay rather than navigating to the URL, which keeps the bytes in an image context even at
 * full size.
 */
function MediaAttachment({
  content,
  resolveUrl,
}: {
  content: MediaRefContent;
  resolveUrl: MediaUrlResolver;
}): ReactNode {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [zoomed, setZoomed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    resolveUrl(content.mediaId)
      .then((resolved) => {
        if (cancelled) {
          return;
        }
        if (resolved === null) {
          setFailed(true);
        } else {
          setUrl(resolved);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setFailed(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [content.mediaId, resolveUrl]);

  const label = mediaLabel(content);

  return (
    <span className="media-attachment">
      {url === null ? (
        <span className="media-pending">
          📎 {label} {failed ? null : <Spinner />}
        </span>
      ) : (
        <button
          type="button"
          className="media-frame"
          onClick={() => setZoomed(true)}
          aria-label="Open image attachment"
        >
          <img src={url} alt="Image attachment" />
        </button>
      )}
      {url !== null && content.caption?.trim() ? (
        <span className="media-caption">{content.caption.trim()}</span>
      ) : null}
      {zoomed && url !== null ? (
        <div
          className="lightbox"
          role="dialog"
          aria-label="Image attachment"
          onClick={() => setZoomed(false)}
        >
          <img src={url} alt="Image attachment" className="lightbox-image" />
        </div>
      ) : null}
    </span>
  );
}

/**
 * Renders the visible text for a message, or a labelled placeholder for non-text content.
 *
 * A media reference with a resolver becomes the embedded image ({@link MediaAttachment}); a voice
 * note becomes the playback bar ({@link VoiceNoteBubble}). Without a resolver both stay their text
 * placeholders, which is also their loading and failed state — that is how a context with no client
 * renders, and the safe fallback if resolving ever stops working.
 */
function renderBody(
  content: MessageContent,
  mediaUrlFor: MediaUrlResolver | undefined,
  onOpenWallet: (() => void) | undefined,
): { node: ReactNode; placeholder: boolean } {
  switch (content.type) {
    case ContentType.Text:
      // A $MIG mention is a live reference to the wallet; without the handler the text renders
      // exactly as it did before — a context with no wallet section loses nothing.
      return {
        node:
          onOpenWallet !== undefined && TICKER_EARLY.test(content.text) ? (
            <TokenText text={content.text} onOpenWallet={onOpenWallet} />
          ) : (
            content.text
          ),
        placeholder: false,
      };
    case ContentType.MediaRef:
      return mediaUrlFor === undefined
        ? { node: `📎 ${mediaLabel(content)}`, placeholder: true }
        : {
            node: <MediaAttachment content={content} resolveUrl={mediaUrlFor} />,
            placeholder: false,
          };
    case ContentType.VoiceNoteRef:
      return mediaUrlFor === undefined
        ? {
            node: `🎤 Voice note (${Math.round(content.durationMs / 1000)}s)`,
            placeholder: true,
          }
        : {
            node: <VoiceNoteBubble content={content} resolveUrl={mediaUrlFor} />,
            placeholder: false,
          };
    case ContentType.Reaction:
      return { node: `Reacted ${content.emoji}`, placeholder: true };
    default:
      return { node: '', placeholder: true };
  }
}

/** Control events are protocol signals, not chat content, so they are never shown. */
function isVisible(message: IncomingMessage): boolean {
  return message.content.type !== ContentType.ControlEvent;
}

/** The display name for a sender: the profile's, "You" for ourselves, a stable fallback otherwise. */
export function senderNameOf(
  senderId: Id,
  selfId: Id,
  profiles: ReadonlyMap<Id, UserProfile>,
): string {
  if (senderId === selfId) {
    return 'You';
  }
  return profiles.get(senderId)?.displayName ?? 'Unknown';
}

/** Two ticks for read, one for sent — text glyphs rather than emoji, so they render everywhere. */
function ReadTicks({ read }: { read: boolean }): ReactNode {
  return (
    <span className={`ticks ${read ? 'read' : 'sent'}`} title={read ? 'Read' : 'Sent'}>
      {read ? '✓✓' : '✓'}
      <span className="visually-hidden">{read ? 'Read' : 'Sent'}</span>
    </span>
  );
}

export interface MessageListProps {
  messages: ThreadMessage[];
  selfId: Id;
  /** Sender names and avatars are a group-conversation affordance; a 1:1 has only one peer. */
  showSenders: boolean;
  /** Resolved sender profiles, for names and avatars. */
  profiles: ReadonlyMap<Id, UserProfile>;
  /** The peer's Read watermark: our messages at or below this seq have been read. */
  readUpTo: number;
  /** Marks a message as the reply target for the composer. */
  onReply: (message: ThreadMessage) => void;
  /** Requests a delete-for-everyone for one of our own messages. */
  onDelete: (messageId: Id) => void;
  /**
   * Commits an edit of one of our own text messages with the replacement text; the caller owns
   * the re-sealing and the `editMessage` call. Optional: without it the Edit control is not
   * offered, which is how a context with no client renders.
   */
  onEdit?: (message: ThreadMessage, text: string) => void;
  /**
   * Sends one of {@link QUICK_REACTIONS} onto a message; the caller owns the sealed reaction
   * envelope. Optional for the same reason as {@link onEdit}.
   */
  onReact?: (message: ThreadMessage, emoji: string) => void;
  /** True while a deletion request is in flight, so its control can show the busy state. */
  deleting: boolean;
  /** Whether the thread holds less than its full history (the initial replay is page-bounded). */
  hasEarlier: boolean;
  /** True while a page of earlier history is being fetched. */
  loadingEarlier: boolean;
  onLoadEarlier: () => void;
  /**
   * Resolves media references to embeddable URLs; when absent every media message stays its text
   * placeholder, which is how a context with no client renders.
   */
  mediaUrlFor?: MediaUrlResolver;
  /**
   * Live rows rendered inside the transcript after the messages — the thread's non-message
   * traffic, e.g. game activity. They scroll with the messages because they are part of the
   * same reading surface; absent for contexts that have none.
   */
  liveSlot?: ReactNode;
  /** How many rows the live slot holds, so auto-scroll follows their arrival too. */
  liveRowCount?: number;
  /**
   * Opens the Wallet section for a $MIG token reference in message text. Optional: without it
   * the text renders without the chips, which is how a context with no wallet section renders.
   */
  onOpenWallet?: () => void;
}

/** How many messages render at once — the render window a large room needs.
 *
 * The spec's "virtualized rendering" for large rooms: the transcript can hold a thousand
 * messages after a few catch-up pages, and rendering every bubble is what makes a busy room
 * stutter on a low-end phone. The window keeps the last {@link RENDER_WINDOW} bubbles in the
 * DOM; "Load earlier" both fetches history and widens the window by one window's worth, so
 * scrolling up is seamless and the DOM stays bounded.
 */
const RENDER_WINDOW = 150;

export function MessageList({
  messages,
  selfId,
  showSenders,
  profiles,
  readUpTo,
  onReply,
  onDelete,
  onEdit,
  onReact,
  deleting,
  hasEarlier,
  loadingEarlier,
  onLoadEarlier,
  mediaUrlFor,
  liveSlot,
  liveRowCount,
  onOpenWallet,
}: MessageListProps): ReactNode {
  const bottomRef = useRef<HTMLDivElement | null>(null);
  // How far back the render window reaches, in messages; grows by one window per Load earlier.
  const [windowSize, setWindowSize] = useState(RENDER_WINDOW);
  const visible = useMemo(() => {
    const drawn = messages.filter(isVisible);
    return drawn.length > windowSize ? drawn.slice(drawn.length - windowSize) : drawn;
  }, [messages, windowSize]);
  // Whether the window is clipping anything — the honest "there is more above" that the
  // Load-earlier control below states even when the server has no more pages to fetch.
  const clipped = useMemo(
    () => messages.filter(isVisible).length > visible.length,
    [messages, visible],
  );
  // The scroll follows both surfaces that can grow: a new message and a new live row are each a
  // reason to bring the bottom into view.
  const liveCount = liveRowCount ?? 0;
  // Which of our own text messages is open in the inline editor, if any. One at a time: an edit
  // is a focused correction, and a second Edit click is a different message's turn.
  const [editingId, setEditingId] = useState<Id | null>(null);

  // Reply quotes resolve their target in the same thread; a target that is absent (hard-deleted,
  // or never loaded) or since tombstoned renders as "[deleted]" rather than an empty quote.
  const byId = useMemo(() => {
    const map = new Map<Id, ThreadMessage>();
    for (const message of visible) {
      map.set(message.messageId, message);
    }
    return map;
  }, [visible]);

  // The scroll follows both surfaces that can grow: a new message and a new live row are each a
  // reason to bring the bottom into view. An empty transcript has nothing to scroll to.
  useEffect(() => {
    if (visible.length > 0 || liveCount > 0) {
      bottomRef.current?.scrollIntoView({ block: 'end' });
    }
  }, [visible.length, liveCount]);

  let lastDay = '';
  let lastSender: Id | null = null;

  return (
    <div className="message-list">
      {hasEarlier || clipped ? (
        <button
          type="button"
          className="btn btn-ghost load-earlier"
          onClick={() => {
            if (hasEarlier) {
              onLoadEarlier();
            }
            // Either way, reveal one more window: a server with no more history can still be
            // holding more than the window shows.
            setWindowSize((size) => size + RENDER_WINDOW);
          }}
          disabled={loadingEarlier}
          aria-live="polite"
        >
          {loadingEarlier ? 'Loading…' : 'Load earlier messages'}
        </button>
      ) : null}
      {visible.map((message) => {
        const dayLabel = formatDayLabel(message.createdAt);
        const showDivider = dayLabel !== lastDay;
        lastDay = dayLabel;
        const mine = message.senderId === selfId;
        // A run of consecutive messages from one sender shows the name and avatar once, on the
        // first; a day divider starts a new run because the block reads as a new conversation turn.
        const startsRun = showDivider || message.senderId !== lastSender;
        lastSender = message.senderId;
        const showHeader = showSenders && !mine && startsRun;
        const senderName = senderNameOf(message.senderId, selfId, profiles);

        const quoted = message.replyTo ? (byId.get(message.replyTo) ?? null) : null;
        const quoteText =
          quoted && !quoted.deleted ? messagePreview(quoted.content, QUOTE_CHARS) : '[deleted]';
        const { node, placeholder } = renderBody(message.content, mediaUrlFor, onOpenWallet);
        const editable = mine && message.content.type === ContentType.Text && onEdit !== undefined;
        const editing = editingId === message.messageId && editable;

        return (
          <div key={message.messageId}>
            {showDivider ? <div className="day-divider">{dayLabel}</div> : null}
            <div className={`bubble-row ${mine ? 'out' : 'in'}`}>
              {showHeader ? (
                <Avatar
                  name={senderName}
                  id={message.senderId}
                  size={26}
                  avatarUrl={profiles.get(message.senderId)?.avatarUrl}
                />
              ) : null}
              <div className="bubble-stack">
                {message.deleted ? (
                  // A tombstone keeps the message's place and timestamp but shows no content: the
                  // row is the proof something was sent and then unsent, which is all a reader
                  // needs to know. No quote, no actions — there is nothing left to act on.
                  <div className="bubble tombstone">
                    <span className="tombstone-text">Message deleted</span>
                    <span className="meta">{formatClock(message.createdAt)}</span>
                  </div>
                ) : (
                  <>
                    {showHeader ? <div className="sender-name">{senderName}</div> : null}
                    {message.replyTo ? (
                      <div className="reply-quote">
                        <span className="reply-quote-name">
                          {quoted ? senderNameOf(quoted.senderId, selfId, profiles) : ''}
                        </span>
                        <span className="reply-quote-text">{quoteText}</span>
                      </div>
                    ) : null}
                    {editing ? (
                      <MessageEditor
                        initialText={
                          message.content.type === ContentType.Text ? message.content.text : ''
                        }
                        busy={false}
                        onSave={(text) => {
                          setEditingId(null);
                          onEdit?.(message, text);
                        }}
                        onCancel={() => setEditingId(null)}
                      />
                    ) : (
                      <BubbleLine
                        message={message}
                        mine={mine}
                        placeholder={placeholder}
                        body={node}
                        read={message.seq <= readUpTo}
                        senderName={senderName}
                        onReply={onReply}
                        onDelete={onDelete}
                        deleting={deleting}
                        editable={editable}
                        onEdit={() => setEditingId(message.messageId)}
                        onReact={onReact}
                      />
                    )}
                  </>
                )}
              </div>
            </div>
          </div>
        );
      })}
      {liveSlot}
      <div ref={bottomRef} />
    </div>
  );
}

/**
 * The inline editor for one of our own text messages: the bubble becomes a textarea with Save
 * and Cancel. The draft starts as the message's current text, Save commits only a change (an
 * untouched draft is a cancel in disguise), and Enter submits while Shift+Enter is a newline.
 */
export function MessageEditor({
  initialText,
  busy,
  onSave,
  onCancel,
}: {
  initialText: string;
  busy: boolean;
  onSave: (text: string) => void;
  onCancel: () => void;
}): ReactNode {
  const [draft, setDraft] = useState(initialText);

  function onChange(event: ChangeEvent<HTMLTextAreaElement>): void {
    setDraft(event.target.value);
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
    if (event.key === 'Escape') {
      onCancel();
    }
  }

  function submit(): void {
    if (busy) {
      return;
    }
    const text = draft.trim();
    if (text.length === 0 || text === initialText) {
      onCancel();
      return;
    }
    onSave(text);
  }

  return (
    <div className="message-editor" aria-label="Edit message">
      <textarea
        className="input"
        value={draft}
        onChange={onChange}
        onKeyDown={onKeyDown}
        rows={2}
        autoFocus
        aria-label="Edited message text"
      />
      <div className="message-editor-actions">
        <button type="button" className="btn btn-primary" onClick={submit} disabled={busy}>
          {busy ? <Spinner /> : 'Save'}
        </button>
        <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/** The quick-reaction hover bar: one button per {@link QUICK_REACTIONS} emoji. */
export function ReactionBar({
  targetName,
  onReact,
}: {
  /** Whose message the reaction lands on, for the accessible label. */
  targetName: string;
  onReact: (emoji: string) => void;
}): ReactNode {
  return (
    <span className="reaction-bar" role="group" aria-label={`React to ${targetName}`}>
      {QUICK_REACTIONS.map((emoji) => (
        <button
          key={emoji}
          type="button"
          className="row-action-btn reaction-btn"
          onClick={() => onReact(emoji)}
          aria-label={`React with ${emoji}`}
        >
          {emoji}
        </button>
      ))}
    </span>
  );
}

/** One bubble with its hover actions (Reply, reactions, and Delete/Edit on our own messages). */
function BubbleLine({
  message,
  mine,
  placeholder,
  body,
  read,
  senderName,
  onReply,
  onDelete,
  deleting,
  editable,
  onEdit,
  onReact,
}: {
  message: ThreadMessage;
  mine: boolean;
  placeholder: boolean;
  body: ReactNode;
  read: boolean;
  senderName: string;
  onReply: (message: ThreadMessage) => void;
  onDelete: (messageId: Id) => void;
  deleting: boolean;
  /** Whether the Edit control is offered at all (own text message, caller can commit edits). */
  editable: boolean;
  onEdit: () => void;
  /** Sends a quick reaction; absent means the bar is not rendered. */
  onReact?: (message: ThreadMessage, emoji: string) => void;
}): ReactNode {
  return (
    <div className="bubble-line">
      <div className={`bubble ${placeholder ? 'placeholder' : ''}`}>
        {body}
        <span className="meta">
          {formatClock(message.createdAt)}
          {message.editedAt !== undefined ? <span className="edited-mark">edited</span> : null}
          {mine ? <ReadTicks read={read} /> : null}
        </span>
      </div>
      <div className="row-actions">
        <button
          type="button"
          className="row-action-btn"
          onClick={() => onReply(message)}
          aria-label={`Reply to ${senderName}`}
          title="Reply"
        >
          ↩
        </button>
        {onReact !== undefined ? (
          <ReactionBar targetName={senderName} onReact={(emoji) => onReact(message, emoji)} />
        ) : null}
        {editable ? (
          <button
            type="button"
            className="row-action-btn"
            onClick={onEdit}
            aria-label="Edit message"
            title="Edit"
          >
            ✎
          </button>
        ) : null}
        {mine ? (
          <button
            type="button"
            className="row-action-btn"
            onClick={() => onDelete(message.messageId)}
            disabled={deleting}
            aria-label="Delete message"
            title="Delete"
          >
            ✕
          </button>
        ) : null}
      </div>
    </div>
  );
}
