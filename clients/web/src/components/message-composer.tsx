'use client';

import { useCallback, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent, ReactNode } from 'react';

/** Stop signalling "typing" after this idle gap. */
const TYPING_IDLE_MS = 2500;

interface ComposerProps {
  onSend: (text: string) => Promise<void>;
  onTyping: (isTyping: boolean) => void;
  disabled?: boolean;
}

export function MessageComposer({ onSend, onTyping, disabled }: ComposerProps): ReactNode {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const typingActiveRef = useRef(false);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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

  function onChange(event: ChangeEvent<HTMLTextAreaElement>): void {
    setText(event.target.value);
    const target = event.target;
    target.style.height = 'auto';
    target.style.height = `${Math.min(target.scrollHeight, 160)}px`;
    if (event.target.value.trim().length > 0) {
      signalTyping();
    } else {
      stopTyping();
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  }

  return (
    <div className="composer">
      <textarea
        value={text}
        onChange={onChange}
        onKeyDown={onKeyDown}
        onBlur={stopTyping}
        placeholder="Write a message…"
        rows={1}
        disabled={disabled}
        aria-label="Message"
      />
      <button
        type="button"
        className="send-btn"
        onClick={() => void submit()}
        disabled={disabled || sending || text.trim().length === 0}
        aria-label="Send"
      >
        ➤
      </button>
    </div>
  );
}
