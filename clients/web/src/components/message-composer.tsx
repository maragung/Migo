'use client';

import { useCallback, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent, ReactNode } from 'react';

import { VOICE_NOTE_MAX_MS } from '@/lib/migo/voice.js';
import type { VoiceRecording } from '@/lib/migo/voice.js';

import { Spinner } from './spinner.js';
import { VoiceRecorder } from './voice-recorder.js';

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
  /**
   * Uploads a finished voice note recording and sends the message that references it. Rejects on
   * failure, so the composer can surface the error beside the mic that started it. Optional: a
   * context without it (no client) renders no mic button at all.
   */
  onVoiceNote?: (recording: VoiceRecording) => Promise<void>;
  onTyping: (isTyping: boolean) => void;
  disabled?: boolean;
  /** The message a send will reply to; the bar is the only surface that shows this is set. */
  replyPreview?: ReplyPreview | null;
  /** Clears the reply target (the bar's X). */
  onCancelReply?: () => void;
  /** Toggles the inline gift picker (the 🎁 beside the attach button). */
  onGift?: () => void;
  /** Whether the gift picker is open, so the control reflects it. */
  giftOpen?: boolean;
}

export function MessageComposer({
  onSend,
  onAttach,
  onVoiceNote,
  onTyping,
  disabled,
  replyPreview,
  onCancelReply,
  onGift,
  giftOpen,
}: ComposerProps): ReactNode {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [voiceRecording, setVoiceRecording] = useState(false);
  const [sendingVoice, setSendingVoice] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);
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

  /**
   * The finished-recording path: swap back to the text composer immediately, then upload and send.
   * A failure keeps the recording's error beside the mic that started it, dismissible — unlike the
   * image line, a voice note the user just recorded is worth an explicit dismissal, not an error
   * that silently disappears on the next keystroke.
   */
  const sendVoiceNote = useCallback(
    async (recording: VoiceRecording): Promise<void> => {
      setSendingVoice(true);
      setVoiceError(null);
      try {
        await onVoiceNote?.(recording);
      } catch {
        setVoiceError('That voice note could not be sent.');
      } finally {
        setSendingVoice(false);
      }
    },
    [onVoiceNote],
  );

  const handleVoiceStop = useCallback(
    (recording: VoiceRecording): void => {
      setVoiceRecording(false);
      void sendVoiceNote(recording);
    },
    [sendVoiceNote],
  );

  const handleVoiceError = useCallback((message: string): void => {
    setVoiceRecording(false);
    setVoiceError(message);
  }, []);

  const handleVoiceCancel = useCallback((): void => {
    setVoiceRecording(false);
  }, []);

  function startVoiceNote(): void {
    setVoiceError(null);
    stopTyping();
    setVoiceRecording(true);
  }

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
      {voiceRecording ? (
        // While a note is captured, the recording bar replaces the input row entirely: no text can
        // be typed into a moment that is being recorded. The reply bar above stays — a voice note
        // replies exactly like an attachment does.
        <VoiceRecorder
          onStop={handleVoiceStop}
          onError={handleVoiceError}
          onCancel={handleVoiceCancel}
          maxDurationMs={VOICE_NOTE_MAX_MS}
        />
      ) : (
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
          {onGift !== undefined ? (
            <button
              type="button"
              className={`attach-btn ${giftOpen ? 'active' : ''}`}
              onClick={onGift}
              disabled={disabled || uploading}
              aria-label={giftOpen ? 'Close gift picker' : 'Send a gift'}
              aria-pressed={giftOpen}
            >
              🎁
            </button>
          ) : null}
          {onVoiceNote !== undefined ? (
            <button
              type="button"
              className="attach-btn"
              onClick={startVoiceNote}
              disabled={disabled || uploading || sending || sendingVoice}
              aria-label="Record a voice note"
            >
              🎤
            </button>
          ) : null}
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
      )}
      {uploading || sendingVoice || uploadError !== null || voiceError !== null ? (
        <div className="composer-meta">
          {uploading ? (
            <>
              <Spinner />
              <span>Uploading image…</span>
            </>
          ) : null}
          {sendingVoice ? (
            <>
              <Spinner />
              <span>Sending voice note…</span>
            </>
          ) : null}
          {uploadError !== null ? <span className="composer-error">{uploadError}</span> : null}
          {voiceError !== null ? (
            <span className="composer-error">
              {voiceError}
              <button
                type="button"
                className="error-dismiss"
                onClick={() => setVoiceError(null)}
                aria-label="Dismiss error"
              >
                ✕
              </button>
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
