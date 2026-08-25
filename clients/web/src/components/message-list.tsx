'use client';

import { useEffect, useRef } from 'react';
import type { ReactNode } from 'react';

import { ContentType } from '@migo/sdk';
import type { Id, IncomingMessage, MessageContent } from '@migo/sdk';

import { formatClock, formatDayLabel } from '@/lib/format.js';

/** Renders the visible text for a message, or a labelled placeholder for non-text content. */
function renderBody(content: MessageContent): { node: ReactNode; placeholder: boolean } {
  switch (content.type) {
    case ContentType.Text:
      return { node: content.text, placeholder: false };
    case ContentType.MediaRef:
      return { node: `📎 ${content.caption?.trim() || 'Attachment'}`, placeholder: true };
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

export function MessageList({
  messages,
  selfId,
}: {
  messages: IncomingMessage[];
  selfId: Id;
}): ReactNode {
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const visible = messages.filter(isVisible);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ block: 'end' });
  }, [messages.length]);

  let lastDay = '';

  return (
    <div className="message-list">
      {visible.map((message) => {
        const dayLabel = formatDayLabel(message.createdAt);
        const showDivider = dayLabel !== lastDay;
        lastDay = dayLabel;
        const mine = message.senderId === selfId;
        const { node, placeholder } = renderBody(message.content);
        return (
          <div key={message.messageId}>
            {showDivider ? <div className="day-divider">{dayLabel}</div> : null}
            <div className={`bubble-row ${mine ? 'out' : 'in'}`}>
              <div className={`bubble ${placeholder ? 'placeholder' : ''}`}>
                {node}
                <span className="meta">{formatClock(message.createdAt)}</span>
              </div>
            </div>
          </div>
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
}
