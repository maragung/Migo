'use client';

import { useCallback, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent, ReactNode } from 'react';

import { Spinner } from './spinner.js';

/** Stop signalling "typing" after this idle gap. */
const TYPING_IDLE_MS = 2500;

/** What the reply bar quotes: who is being replied to, and the start of their message. */
export interface ReplyPreview {
  senderName: string;
  snippet: string;
}

interface ComposerProps {
  onSend: (text: string) => Promise<void>;
  /**
   * Uploads an attached image and sends the message that references it. Rejects on failure, so the
   * composer can surface the error beside the input it belongs to.
   */
  onAttach: (file: File) => Promise<void>;
  onTyping: (isTyping: boolean) => void;
  disabled?: boolean;
  /** The message a send will reply to; the bar is the only surface that shows this is set. */
  replyPreview?: ReplyPreview | null;
  /** Clears the reply target (the bar's X). */
  onCancelReply?: () => void;
}

export function MessageComposer({
  onSend,
  onAttach,
  onTyping,
  disabled,
  replyPreview,
  onCancelReply,
}: ComposerProps): ReactNode {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const typingActiveRef = useRef(false);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  const stopTyping = useCallback((): void => {
    if (idleTimerRef.current) {
      clearTimeout(idleTimerRef.current);
      idleTimerRef.current = null;
    }
    if (typingActiveRef.current) {
      typingActiveRef.current = false;
      onTyping(false);
    }
  }, [onTyping]);

  const signalTyping = useCallback((): void => {
    if (!typingActiveRef.current) {
      typingActiveRef.current = true;
      onTyping(true);
    }
    if (idleTimerRef.current) {
      clearTimeout(idleTimerRef.current);
    }
    idleTimerRef.current = setTimeout(stopTyping, TYPING_IDLE_MS);
  }, [onTyping, stopTyping]);

  const submit = useCallback(async (): Promise<void> => {
    const value = text.trim();
    if (value.length === 0 || sending) {
      return;
    }
    setSending(true);
    stopTyping();
    try {
      await onSend(value);
      setText('');
    } catch {
      // Keep the text in the box so the user can retry.
    } finally {
      setSending(false);
    }
  }, [text, sending, onSend, stopTyping]);

  const attach = useCallback(
    async (file: File): Promise<void> => {
      setUploading(true);
      setUploadError(null);
      stopTyping();
      try {
        await onAttach(file);
      } catch {
        // The upload failed before any message was sent; say so beside the picker that started it.
        setUploadError('That image could not be sent.');
      } finally {
        setUploading(false);
      }
    },
    [onAttach, stopTyping],
  );

  function onChange(event: ChangeEvent<HTMLTextAreaElement>): void {
    setText(event.target.value);
    setUploadError(null);
    const target = event.target;
    target.style.height = 'auto';
    target.style.height = `${Math.min(target.scrollHeight, 160)}px`;
    if (event.target.value.trim().length > 0) {
      signalTyping();
    } else {
      stopTyping();
    }
  }

  function onFileChange(event: ChangeEvent<HTMLInputElement>): void {
    const file = event.target.files?.[0];
    // Reset the input so picking the same file again still fires a change event.
    event.target.value = '';
    if (file === undefined || uploading) {
      return;
    }
    void attach(file);
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  }

  return (
    <div className="composer-wrap">
      {replyPreview ? (
        <div className="reply-bar" aria-live="polite">
          <span className="reply-bar-label">
            Replying to <strong>{replyPreview.senderName}</strong>
            <span className="reply-bar-snippet">: {replyPreview.snippet}</span>
          </span>
          <button
            type="button"
            className="icon-btn reply-cancel"
            onClick={onCancelReply}
            aria-label="Cancel reply"
          >
            ✕
          </button>
        </div>
      ) : null}
      <div className="composer">
        <textarea
          value={text}
          onChange={onChange}
          onKeyDown={onKeyDown}
          onBlur={stopTyping}
          placeholder="Write a message…"
          rows={1}
          disabled={disabled || uploading}
          aria-label="Message"
        />
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          onChange={onFileChange}
          hidden
          aria-label="Attach an image"
        />
        <button
          type="button"
          className="attach-btn"
          onClick={() => fileInputRef.current?.click()}
          disabled={disabled || uploading}
          aria-label="Attach an image"
        >
          📎
        </button>
        <button
          type="button"
          className="send-btn"
          onClick={() => void submit()}
          disabled={disabled || sending || uploading || text.trim().length === 0}
          aria-label="Send"
        >
          ➤
        </button>
      </div>
      {uploading || uploadError !== null ? (
        <div className="composer-meta">
          {uploading ? (
            <>
              <Spinner />
              <span>Uploading image…</span>
            </>
          ) : null}
          {uploadError !== null ? <span className="composer-error">{uploadError}</span> : null}
        </div>
      ) : null}
    </div>
  );
}
