'use client';

/**
 * The voice note bubble: a play/pause control, a waveform strip, a duration label.
 *
 * Playback is imperative on purpose — a `new Audio(url)` created on click, never an `<audio>`
 * element in the tree. The element would have to render before any URL resolved, and, the sharper
 * edge, a voice note is sender-shaped content under section 122: the claimed `mimeType` is never
 * acted on and never printed, the waveform's *length* is normalised before it becomes DOM (a
 * hostile 100,000-bar blob must not render 100,000 elements), and the only server-controlled value
 * that touches this component is the media id, which goes to `resolveUrl` and comes back as a URL
 * that is set on the audio element and shown nowhere else — not in markup, not in an error, not in
 * a log line.
 *
 * # URL expiry mid-playback
 *
 * A signed URL outlives its grant by nothing, and a note can play for longer than the grant
 * lasts. An audio error therefore re-resolves the URL and resumes from the last reported position,
 * once — the second failure is a real failure and shows the fallback label. (A URL near expiry is
 * normally replaced by the session cache before this ever bites; the retry is for the gap between
 * "near" and "past".)
 *
 * # Duration
 *
 * MediaRecorder-produced webm reports `duration: Infinity` in Chrome until it is remuxed, so the
 * progress denominator falls back to the message's claimed `durationMs` — the sender's own clock —
 * whenever the element has no finite duration to offer.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { VoiceNoteRefContent } from '@migo/sdk';

import { WAVEFORM_BARS, downsampleWaveform, formatDuration } from '@/lib/migo/voice.js';

import { Spinner } from './spinner.js';
import type { MediaUrlResolver } from './message-list.js';

type PlaybackStatus = 'idle' | 'loading' | 'playing' | 'paused' | 'failed';

interface VoiceNoteBubbleProps {
  content: VoiceNoteRefContent;
  /** Resolves the media id to a fetchable URL; `null` means the object cannot be resolved. */
  resolveUrl: MediaUrlResolver;
}

export function VoiceNoteBubble({ content, resolveUrl }: VoiceNoteBubbleProps): ReactNode {
  const [status, setStatus] = useState<PlaybackStatus>('idle');
  const [progress, setProgress] = useState(0);

  const audioRef = useRef<HTMLAudioElement | null>(null);
  /** The playback position as of the last timeupdate, so a retried fetch resumes where it left off. */
  const positionRef = useRef(0);
  /** Whether the one re-fetch after an audio error has been spent. */
  const retriedRef = useRef(false);
  /** Guards async playback starts against being superseded by a newer click or an unmount. */
  const playTokenRef = useRef(0);
  /** The latest `playFresh`, so an error handler defined before it can still start a retry. */
  const playFreshRef = useRef<(resumeAtMs: number) => Promise<void>>(() => Promise.resolve());

  /**
   * The progress denominator: the element's own duration when it has a finite one, the message's
   * claimed duration otherwise (streamed webm often reports Infinity — see the module doc).
   */
  const totalSecondsOf = useCallback(
    (audio: HTMLAudioElement): number =>
      Number.isFinite(audio.duration) && audio.duration > 0
        ? audio.duration
        : content.durationMs / 1000,
    [content.durationMs],
  );

  /** Drops the live audio element: listeners off, playback stopped, source released. */
  const teardownAudio = useCallback((): void => {
    const audio = audioRef.current;
    audioRef.current = null;
    if (audio === null) {
      return;
    }
    audio.ontimeupdate = null;
    audio.onended = null;
    audio.onerror = null;
    audio.pause();
    // Drop the source without firing the error an empty src would; the handlers are already off.
    audio.removeAttribute('src');
    audio.load();
  }, []);

  /** Resolves the URL, builds a fresh audio element on it, and plays — resuming if asked to. */
  const playFresh = useCallback(
    async (resumeAtMs: number): Promise<void> => {
      const token = playTokenRef.current + 1;
      playTokenRef.current = token;
      setStatus('loading');

      let url: string | null;
      try {
        url = await resolveUrl(content.mediaId);
      } catch {
        url = null;
      }
      if (playTokenRef.current !== token) {
        // A newer click (or the unmount) owns the state now; this start is dead.
        return;
      }
      if (url === null) {
        setStatus('failed');
        return;
      }

      const audio = new Audio(url);
      const attach = (): void => {
        audio.ontimeupdate = () => {
          if (playTokenRef.current !== token || audioRef.current !== audio) {
            return;
          }
          positionRef.current = audio.currentTime * 1000;
          const total = totalSecondsOf(audio);
          setProgress(total > 0 ? Math.min(1, audio.currentTime / total) : 0);
        };
        audio.onended = () => {
          if (playTokenRef.current !== token || audioRef.current !== audio) {
            return;
          }
          teardownAudio();
          positionRef.current = 0;
          setProgress(0);
          setStatus('idle');
        };
        audio.onerror = () => {
          if (playTokenRef.current !== token || audioRef.current !== audio) {
            return;
          }
          // The URL grant likely expired under playback: re-resolve and resume once (module doc).
          const at = positionRef.current;
          teardownAudio();
          if (retriedRef.current) {
            setStatus('failed');
            return;
          }
          retriedRef.current = true;
          void playFreshRef.current(at);
        };
      };
      attach();
      audioRef.current = audio;
      if (resumeAtMs > 0) {
        audio.currentTime = resumeAtMs / 1000;
      }
      try {
        await audio.play();
        if (playTokenRef.current !== token) {
          teardownAudio();
          return;
        }
        setStatus('playing');
      } catch {
        // play() rejecting with no error-event retry of our own having run is a real failure
        // (codec, autoplay policy); the error handler has already handled the load-failure case
        // by replacing or dropping the element, which the token/element checks below reflect.
        if (playTokenRef.current === token && audioRef.current === audio) {
          teardownAudio();
          setStatus('failed');
        }
      }
    },
    [content.mediaId, resolveUrl, teardownAudio, totalSecondsOf],
  );
  playFreshRef.current = playFresh;

  /** Play/pause toggle: resumes a paused element, otherwise starts a fresh playback session. */
  const toggle = useCallback((): void => {
    const audio = audioRef.current;
    if (audio !== null) {
      if (audio.paused) {
        void audio
          .play()
          .then(() => setStatus('playing'))
          .catch(() => {
            teardownAudio();
            setStatus('failed');
          });
      } else {
        audio.pause();
        setStatus('paused');
      }
      return;
    }
    // A new session from a dead element restarts the retry budget and the position: the one
    // automatic retry already resumed where playback broke, so this click is a deliberate restart.
    retriedRef.current = false;
    positionRef.current = 0;
    setProgress(0);
    void playFresh(0);
  }, [playFresh, teardownAudio]);

  // Unmount: invalidate any in-flight start, then release the element. Position state is
  // component-local, so each mediaId's player is independent — several notes in one thread each
  // keep their own progress, and only their own.
  useEffect(
    () => () => {
      playTokenRef.current += 1;
      teardownAudio();
    },
    [teardownAudio],
  );

  /**
   * The bars to draw: the message's waveform when it has one, normalised to at most the display
   * width (a sender controls this length; the DOM must not), and `null` when there is nothing to
   * draw — the bubble then falls back to a plain progress bar.
   */
  const bars = useMemo(() => {
    const wave = content.waveform;
    if (wave === undefined || wave.length === 0) {
      return null;
    }
    return wave.length > WAVEFORM_BARS ? downsampleWaveform(wave, WAVEFORM_BARS) : wave;
  }, [content.waveform]);

  const playing = status === 'playing';
  const loading = status === 'loading';
  const playedBars = bars === null ? 0 : Math.floor(progress * bars.length);

  return (
    <span className="voice-note">
      <button
        type="button"
        className="voice-play-btn"
        onClick={toggle}
        disabled={loading}
        aria-label={playing ? 'Pause voice note' : 'Play voice note'}
      >
        {loading ? <Spinner /> : playing ? '❚❚' : '▶'}
      </button>
      {bars !== null ? (
        <span className="voice-wave" aria-hidden="true">
          {Array.from(bars, (value, index) => (
            <span
              key={index}
              className={`voice-bar${index < playedBars ? ' played' : ''}`}
              style={{ height: `${2 + Math.round((value / 255) * 22)}px` }}
            />
          ))}
        </span>
      ) : (
        <span className="voice-progress" aria-hidden="true">
          <span
            className="voice-progress-fill"
            style={{ width: `${Math.round(progress * 100)}%` }}
          />
        </span>
      )}
      <span className="voice-duration">{formatDuration(content.durationMs)}</span>
      {status === 'failed' ? <span className="voice-failed">Unavailable</span> : null}
    </span>
  );
}
