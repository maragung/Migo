'use client';

/**
 * The voice note bubble: a play/pause control, a waveform strip, a duration label.
 *
 * Playback is imperative on purpose — a `new Audio(url)` created on click, never an `<audio>`
 * element in the tree. The element would have to render before any URL resolved, and, the sharper
 * edge, a voice note is sender-shaped content under section 122: the claimed `mimeType` is never
 * printed and reaches no decoder except as the blob label the resolver chose, the waveform's
 * *length* is normalised before it becomes DOM (a hostile 100,000-bar blob must not render
 * 100,000 elements), and the sender-controlled values that leave this component are the message
 * content's key, nonce, and claim — which go to `resolveUrl` and come back as one object URL that
 * is set on the audio element and shown nowhere else — not in markup, not in an error, not in a
 * log line.
 *
 * # URL expiry mid-playback
 *
 * The signed URL the object is fetched through outlives its grant by nothing — but the object URL
 * playback holds is the *decrypted* bytes already in hand, which never expire. An audio error is
 * therefore retried by re-resolving the object (a re-download, and a second open of bytes this
 * component may have already played) and resuming from the last reported position, once — the
 * second failure is a real failure and shows the fallback label. (A blob URL is cached per media
 * id for the session, so in practice the retry is for a browser that dropped the blob, not for
 * the network.)
 *
 * # Duration
 *
 * MediaRecorder-produced webm reports `duration: Infinity` in Chrome until it is remuxed, so the
 * progress denominator falls back to the message's claimed `durationMs` — the sender's own clock —
 * whenever the element has no finite duration to offer.
 *
 * # Receiver-local state (section 179)
 *
 * The speed control cycles 1× → 1.5× → 2× client-side only: the rate is set on the `Audio` element
 * already holding the decrypted bytes, never by fetching the media again, and a switch mid-playback
 * keeps `currentTime` — `playbackRate` resets nothing. The listened mark is likewise
 * receiver-local: a play that reaches {@link LISTENED_THRESHOLD} of the duration (or ends) marks
 * the note heard, and the row's toggle can mark it heard or unheard by hand. Both live in
 * `voice-note-state.ts`, never on the wire — the sender learns neither, and "unlistened" cancels
 * no receipt.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { Id, VoiceNoteRefContent } from '@migo/sdk';

import { WAVEFORM_BARS, downsampleWaveform, formatDuration } from '@/lib/migo/voice.js';
import {
  LISTENED_THRESHOLD,
  formatPlaybackSpeed,
  markVoiceNoteListened,
  nextPlaybackSpeed,
  useVoiceNoteListened,
  useVoicePlaybackSpeed,
} from '@/lib/migo/voice-note-state.js';

import { Icon } from './icons.js';
import { Spinner } from './spinner.js';
import type { MediaObjectResolver } from './message-list.js';

type PlaybackStatus = 'idle' | 'loading' | 'playing' | 'paused' | 'failed';

interface VoiceNoteBubbleProps {
  content: VoiceNoteRefContent;
  /**
   * Resolves the whole reference — download, open with the message's key slots for a sealed note,
   * pass through for a legacy one — to an object URL; `null` means the object cannot be resolved.
   */
  resolveUrl: MediaObjectResolver;
  /**
   * The message's id, present only for a *received* note whose listen state this receiver tracks.
   * Our own recordings render without the heard indicator and never auto-mark — the sender has
   * heard their own note by construction, and this state is receiver-local by design.
   */
  listenMessageId?: Id;
}

export function VoiceNoteBubble({
  content,
  resolveUrl,
  listenMessageId,
}: VoiceNoteBubbleProps): ReactNode {
  const [status, setStatus] = useState<PlaybackStatus>('idle');
  const [progress, setProgress] = useState(0);

  const [speed, setSpeed] = useVoicePlaybackSpeed();
  const listen = useVoiceNoteListened(listenMessageId);
  const listened = listen !== null && listen[0];

  const audioRef = useRef<HTMLAudioElement | null>(null);
  /** The playback position as of the last timeupdate, so a retried fetch resumes where it left off. */
  const positionRef = useRef(0);
  /** Whether the one re-fetch after an audio error has been spent. */
  const retriedRef = useRef(false);
  /** Guards async playback starts against being superseded by a newer click or an unmount. */
  const playTokenRef = useRef(0);
  /** The latest `playFresh`, so an error handler defined before it can still start a retry. */
  const playFreshRef = useRef<(resumeAtMs: number) => Promise<void>>(() => Promise.resolve());
  /** The current speed, so a closure built once per playback session always sets the latest rate. */
  const speedRef = useRef(speed);
  speedRef.current = speed;
  /** The tracked message id and its current mark, for the timeupdate/ended listeners above. */
  const listenIdRef = useRef(listenMessageId);
  listenIdRef.current = listenMessageId;
  const listenedRef = useRef(listened);
  listenedRef.current = listened;

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

  /** Marks the note heard when it is a tracked one and not already marked: the auto path. */
  const markListenedNow = useCallback((): void => {
    const id = listenIdRef.current;
    if (id !== undefined && !listenedRef.current) {
      // Receiver-local only (see the module doc): a store write and an announcement, never a
      // receipt — the sender's transcript cannot learn this happened.
      markVoiceNoteListened(id, true);
    }
  }, []);

  /** Resolves the URL, builds a fresh audio element on it, and plays — resuming if asked to. */
  const playFresh = useCallback(
    async (resumeAtMs: number): Promise<void> => {
      const token = playTokenRef.current + 1;
      playTokenRef.current = token;
      setStatus('loading');

      let url: string | null;
      try {
        url = await resolveUrl(content);
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
      // The persisted rate rides every fresh element, so a note started after a speed change
      // already plays at the listener's choice. Setting it here moves nothing else: no load, no
      // position reset, and the bytes are the ones already in hand.
      audio.playbackRate = speedRef.current;
      const attach = (): void => {
        audio.ontimeupdate = () => {
          if (playTokenRef.current !== token || audioRef.current !== audio) {
            return;
          }
          positionRef.current = audio.currentTime * 1000;
          const total = totalSecondsOf(audio);
          const ratio = total > 0 ? Math.min(1, audio.currentTime / total) : 0;
          setProgress(ratio);
          // Near the end is heard (module doc): the threshold check runs on the ratio the bar
          // already computes, so the mark costs nothing extra per tick.
          if (ratio >= LISTENED_THRESHOLD) {
            markListenedNow();
          }
        };
        audio.onended = () => {
          if (playTokenRef.current !== token || audioRef.current !== audio) {
            return;
          }
          // The end event is heard even when the duration denominator was the sender's claim —
          // completion is a fact about playback, not an estimate.
          markListenedNow();
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
    [content, resolveUrl, teardownAudio, totalSecondsOf, markListenedNow],
  );
  playFreshRef.current = playFresh;

  // A speed change under live playback is applied in place: `playbackRate` on an existing element
  // keeps `currentTime` (the spec resets nothing), so the switch never restarts the note and never
  // asks the network for the bytes again — the §167 rule that speed is purely client-side.
  useEffect(() => {
    const audio = audioRef.current;
    if (audio !== null) {
      audio.playbackRate = speed;
    }
  }, [speed]);

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
  const speedLabel = formatPlaybackSpeed(speed);

  return (
    <span
      className={`voice-note${listen !== null ? (listened ? ' listened' : ' unlistened') : ''}`}
    >
      {listen !== null ? (
        // The heard mark is the receiver's own memory, drawn as a mic that empties once the note
        // is heard — bright while unheard, dim after. It is not a receipt: no sender ever sees it.
        <span
          className="voice-heard"
          role="img"
          aria-label={listened ? 'Listened' : 'Not listened yet'}
          title={listened ? 'Listened' : 'Not listened yet'}
        >
          <Icon name="mic" size={10} />
        </span>
      ) : null}
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
      <button
        type="button"
        className="voice-speed-btn"
        onClick={() => setSpeed(nextPlaybackSpeed(speed))}
        aria-label={`Playback speed ${speedLabel}, tap for ${formatPlaybackSpeed(nextPlaybackSpeed(speed))}`}
        title="Playback speed"
      >
        {speedLabel}
      </button>
      <span className="voice-duration">{formatDuration(content.durationMs)}</span>
      {status === 'failed' ? <span className="voice-failed">Unavailable</span> : null}
    </span>
  );
}

/**
 * The row action for one received voice note: marks it listened or unlistened by hand.
 *
 * The mic reads bright while the note is unheard and dims once it is marked, so the hover bar and
 * the bubble's own heard mark tell one story. The flip is a purely local write (see the bubble's
 * module doc): marking a note unlistened hides nothing the sender was told — it only restores the
 * receiver's own "not heard yet" reminder. Rendered only for received notes, so it can never be
 * mistaken for the sender-side read ticks our own messages carry.
 *
 * The glyph is the `🎤` text the voice-note placeholder already uses — not the Icon family's svg —
 * because this action renders on the placeholder path too (a context with no client), and the
 * section 122 contract pinned in `media-render.test.tsx` keeps that whole path free of live
 * `<svg>` elements: the only markup a sender-controlled note may produce there is escaped text.
 */
export function VoiceListenToggle({ messageId }: { messageId: Id }): ReactNode {
  const listen = useVoiceNoteListened(messageId);
  if (listen === null) {
    return null;
  }
  const [listened, mark] = listen;
  return (
    <button
      type="button"
      className={`row-action-btn voice-listen${listened ? ' listened' : ''}`}
      onClick={() => mark(!listened)}
      aria-label={listened ? 'Mark voice note as unlistened' : 'Mark voice note as listened'}
      title={listened ? 'Mark as unlistened' : 'Mark as listened'}
    >
      🎤
    </button>
  );
}
