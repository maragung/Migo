'use client';

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { ContentType } from '@migo/sdk';
import type { Id, IncomingMessage, MediaRefContent, MessageContent, UserProfile } from '@migo/sdk';

import { formatClock, formatDayLabel } from '@/lib/format.js';
import { messagePreview } from '@/lib/message-preview.js';
import type { ThreadMessage } from '@/lib/migo/use-chat.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** How much of a quoted message the reply snippet above a bubble shows. */
const QUOTE_CHARS = 60;

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
 * A media reference with a resolver becomes the embedded image ({@link MediaAttachment}); without
 * one it stays the text placeholder, which is also its loading and failed state.
 */
function renderBody(
  content: MessageContent,
  mediaUrlFor: MediaUrlResolver | undefined,
): { node: ReactNode; placeholder: boolean } {
  switch (content.type) {
    case ContentType.Text:
      return { node: content.text, placeholder: false };
    case ContentType.MediaRef:
      return mediaUrlFor === undefined
        ? { node: `📎 ${mediaLabel(content)}`, placeholder: true }
        : {
            node: <MediaAttachment content={content} resolveUrl={mediaUrlFor} />,
            placeholder: false,
          };
    case ContentType.VoiceNoteRef:
      return {
        node: `🎤 Voice note (${Math.round(content.durationMs / 1000)}s)`,
        placeholder: true,
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
}

export function MessageList({
  messages,
  selfId,
  showSenders,
  profiles,
  readUpTo,
  onReply,
  onDelete,
  deleting,
  hasEarlier,
  loadingEarlier,
  onLoadEarlier,
  mediaUrlFor,
}: MessageListProps): ReactNode {
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const visible = useMemo(() => messages.filter(isVisible), [messages]);

  // Reply quotes resolve their target in the same thread; a target that is absent (hard-deleted,
  // or never loaded) or since tombstoned renders as "[deleted]" rather than an empty quote.
  const byId = useMemo(() => {
    const map = new Map<Id, ThreadMessage>();
    for (const message of visible) {
      map.set(message.messageId, message);
    }
    return map;
  }, [visible]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ block: 'end' });
  }, [messages.length]);

  let lastDay = '';
  let lastSender: Id | null = null;

  return (
    <div className="message-list">
      {hasEarlier ? (
        <button
          type="button"
          className="btn btn-ghost load-earlier"
          onClick={onLoadEarlier}
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
        const { node, placeholder } = renderBody(message.content, mediaUrlFor);

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
                    />
                  </>
                )}
              </div>
            </div>
          </div>
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
}

/** One bubble with its hover actions (Reply, and Delete on our own messages). */
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
}): ReactNode {
  return (
    <div className="bubble-line">
      <div className={`bubble ${placeholder ? 'placeholder' : ''}`}>
        {body}
        <span className="meta">
          {formatClock(message.createdAt)}
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
