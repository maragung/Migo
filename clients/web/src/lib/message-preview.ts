/**
 * One-line previews of message content, shared by every surface that quotes a message without
 * rendering it whole: the conversation list's last-message line, the reply quote above a bubble,
 * and the composer's reply bar.
 *
 * The rules mirror {@link renderBody} in the message list, because the two must never disagree
 * about what a message "is" — a preview that said `image/png` while the bubble said "Attachment"
 * would leak the sender's claimed mime type on a surface the message-list tests police. Every
 * non-text kind therefore reduces to the same fixed placeholder the bubble uses, and text is the
 * only content whose body is shown, truncated with an ellipsis so a fixed-height row stays fixed.
 */

import { ContentType } from '@migo/sdk';
import type { MessageContent } from '@migo/sdk';

/** Appended when a preview had to be cut short, so a truncated line reads as truncated. */
const ELLIPSIS = '…';

/** A short single-line stand-in for a message body, truncated to `maxChars`. */
export function messagePreview(content: MessageContent, maxChars: number): string {
  return truncate(previewText(content), maxChars);
}

/**
 * Clips a single line to `maxChars` characters on a word boundary where one is nearby, appending
 * an ellipsis. Exported for lines that combine several parts (a name prefix plus a preview) and so
 * must be truncated as a whole rather than piecewise.
 */
export function truncate(text: string, maxChars: number): string {
  if (text.length <= maxChars) {
    return text;
  }
  const cut = text.slice(0, Math.max(0, maxChars - ELLIPSIS.length));
  const lastSpace = cut.lastIndexOf(' ');
  const trimmed = lastSpace > maxChars / 2 ? cut.slice(0, lastSpace) : cut;
  return `${trimmed.trimEnd()}${ELLIPSIS}`;
}

/**
 * The un-truncated preview text for a content type.
 *
 * Media and voice previews keep the emoji prefixes the message list's bubbles already use, so the
 * sidebar and the transcript describe the same message the same way.
 */
function previewText(content: MessageContent): string {
  switch (content.type) {
    case ContentType.Text:
      return content.text.trim();
    case ContentType.MediaRef:
      return `📎 ${content.caption?.trim() || 'Attachment'}`;
    case ContentType.VoiceNoteRef:
      return `🎤 Voice note (${Math.round(content.durationMs / 1000)}s)`;
    case ContentType.Reaction:
      return `Reacted ${content.emoji}`;
    default:
      return 'Message';
  }
}
