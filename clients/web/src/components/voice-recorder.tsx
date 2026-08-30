'use client';

/**
 * The recording bar: what the composer becomes while a voice note is being captured.
 *
 * Mounting *is* the instruction to record — the composer swaps this component in for its input row
 * when the mic button is clicked, and the component owns the whole browser session: permission,
 * `MediaRecorder`, the `AudioContext` graph that samples the waveform, the timer, and the
 * five-minute cap. None of that can run under Node (there is no `MediaRecorder` there), so the pure
 * parts — the waveform fold, the duration format, the mime claim — live in `lib/migo/voice.ts` and
 * this file stays the thin, browser-only shell around them.
 *
 * # Lifecycle
 *
 * {@link VoiceRecorderProps.onStop} is the success path: one finished `VoiceRecording`, already
 * capped and waveformed, for the composer to upload and send. `onError` is every failure — no
 * `MediaRecorder`, permission refused, a recorder error — after which the component has already
 * torn its session down and hands the composer a sentence to show inline. `onCancel` is the user
 * backing out while the permission prompt is still pending. Unmount releases everything without
 * calling any callback: the conversation view going away mid-recording must not send anything.
 *
 * # Cleanup
 *
 * Every resource is released on exactly one path — the effect teardown and the recorder's own
 * `onstop` share it: the sampling interval is cleared, every audio track on the stream is stopped
 * (the mic light goes off), the `AudioContext` is closed, and the recorder is detached from its
 * handlers before a silent `stop()`. No object URL is ever minted for the blob; its bytes go
 * straight to `arrayBuffer()` at upload, so there is nothing left to revoke.
 */

import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import {
  WAVEFORM_BARS,
  downsampleWaveform,
  formatDuration,
  normalizeVoiceMime,
  pickRecorderMimeType,
} from '@/lib/migo/voice.js';
import type { VoiceRecording } from '@/lib/migo/voice.js';

import { Spinner } from './spinner.js';

/** How often the amplitude is sampled (and the timer re-rendered) while recording. */
const SAMPLE_INTERVAL_MS = 100;

/** The recorder is asked for a chunk this often, so a crash costs at most this much audio. */
const CHUNK_INTERVAL_MS = 500;

export interface VoiceRecorderProps {
  /** Delivers the finished recording; the composer uploads and sends it. */
  onStop: (recording: VoiceRecording) => void;
  /** Reports a failure (unsupported browser, denied permission, recorder error) for inline display. */
  onError: (message: string) => void;
  /** The user backed out before anything was captured. */
  onCancel: () => void;
  /** The hard cap on recording length; the bar stops itself at the deadline. */
  maxDurationMs: number;
}

/**
 * The bar itself: a pulsing red dot, a monospace `M:SS` timer, and the stop control. While
 * permission is still pending the dot and timer are replaced by a "requesting" state, and the stop
 * control becomes a cancel.
 */
export function VoiceRecorder({
  onStop,
  onError,
  onCancel,
  maxDurationMs,
}: VoiceRecorderProps): ReactNode {
  const [phase, setPhase] = useState<'starting' | 'recording' | 'stopping'>('starting');
  const [elapsedMs, setElapsedMs] = useState(0);

  // Live callbacks in refs: a recording session must not restart (or drop its completion) because
  // a parent re-rendered with a fresh callback identity.
  const onStopRef = useRef(onStop);
  onStopRef.current = onStop;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;

  const recorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const samplerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const samplesRef = useRef<number[]>([]);
  const chunksRef = useRef<Blob[]>([]);
  const startedAtRef = useRef(0);
  /** Set when the user cancels while the permission prompt is still pending. */
  const abandonedRef = useRef(false);
  const requestStopRef = useRef<() => void>(() => {});

  useEffect(() => {
    let cancelled = false;

    /** Releases the session's shared resources: interval, mic stream, audio context. */
    const releaseSession = (): void => {
      if (samplerRef.current !== null) {
        clearInterval(samplerRef.current);
        samplerRef.current = null;
      }
      for (const track of streamRef.current?.getTracks() ?? []) {
        track.stop();
      }
      streamRef.current = null;
      analyserRef.current = null;
      const context = audioContextRef.current;
      audioContextRef.current = null;
      if (context !== null) {
        void context.close().catch(() => {
          // Closing is cleanup; a context that refuses to close is released with the page anyway.
        });
      }
    };

    requestStopRef.current = (): void => {
      const recorder = recorderRef.current;
      if (recorder === null || recorder.state !== 'recording') {
        return;
      }
      setPhase('stopping');
      recorder.stop();
    };

    async function begin(): Promise<void> {
      if (
        typeof MediaRecorder === 'undefined' ||
        navigator.mediaDevices?.getUserMedia === undefined
      ) {
        onErrorRef.current('Voice notes are not supported in this browser.');
        return;
      }
      let stream: MediaStream;
      try {
        stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      } catch {
        if (!cancelled && !abandonedRef.current) {
          onErrorRef.current('Microphone access was denied.');
        }
        return;
      }
      if (cancelled || abandonedRef.current) {
        for (const track of stream.getTracks()) {
          track.stop();
        }
        return;
      }
      streamRef.current = stream;

      const preferred = pickRecorderMimeType();
      let recorder: MediaRecorder;
      try {
        recorder =
          preferred === ''
            ? new MediaRecorder(stream)
            : new MediaRecorder(stream, { mimeType: preferred });
      } catch {
        releaseSession();
        onErrorRef.current('Voice notes are not supported in this browser.');
        return;
      }
      recorderRef.current = recorder;

      // The waveform graph is best-effort: a context that cannot be created costs the preview, not
      // the recording — the note then ships without a waveform and the player shows a progress bar.
      try {
        const context = new AudioContext();
        const analyser = context.createAnalyser();
        analyser.fftSize = 2048;
        // Analysing, not monitoring: the graph never reaches the destination, so no echo.
        context.createMediaStreamSource(stream).connect(analyser);
        audioContextRef.current = context;
        analyserRef.current = analyser;
      } catch {
        // Sampled amplitude stays empty; the recording proceeds.
      }

      chunksRef.current = [];
      samplesRef.current = [];
      startedAtRef.current = performance.now();

      recorder.ondataavailable = (event: BlobEvent) => {
        if (event.data.size > 0) {
          chunksRef.current.push(event.data);
        }
      };
      recorder.onstop = () => {
        releaseSession();
        const blob = new Blob(chunksRef.current, { type: recorder.mimeType });
        const recording: VoiceRecording = {
          blob,
          mimeType: normalizeVoiceMime(blob.type === '' ? recorder.mimeType : blob.type),
          // The cap is applied to the reported length, not just the clock: an auto-stop at the
          // deadline lands a few sampling ticks past it, and the server refuses anything longer.
          durationMs: Math.min(Math.round(performance.now() - startedAtRef.current), maxDurationMs),
          waveform:
            samplesRef.current.length > 0
              ? downsampleWaveform(samplesRef.current, WAVEFORM_BARS)
              : undefined,
        };
        if (!cancelled) {
          onStopRef.current(recording);
        }
      };
      recorder.onerror = () => {
        recorder.onstop = null;
        releaseSession();
        if (!cancelled) {
          onErrorRef.current('The recording failed before it finished.');
        }
      };
      recorder.start(CHUNK_INTERVAL_MS);

      // One clock drives the displays and the cap: sample the amplitude, advance the timer, and
      // stop the moment the server's limit is reached — what the composer is handed is never
      // longer than the server would accept.
      const amplitudes = new Uint8Array(analyserRef.current?.fftSize ?? 0);
      samplerRef.current = setInterval(() => {
        const analyser = analyserRef.current;
        if (analyser !== null) {
          analyser.getByteTimeDomainData(amplitudes);
          let peak = 0;
          for (const value of amplitudes) {
            const deviation = Math.abs(value - 128);
            if (deviation > peak) {
              peak = deviation;
            }
          }
          // Time-domain bytes centre on 128; a full-scale swing (±128) scales to the 255 bar.
          samplesRef.current.push(Math.min(255, peak * 2));
        }
        const elapsed = performance.now() - startedAtRef.current;
        setElapsedMs(elapsed);
        if (elapsed >= maxDurationMs) {
          requestStopRef.current();
        }
      }, SAMPLE_INTERVAL_MS);
      setPhase('recording');
    }

    void begin();

    return () => {
      cancelled = true;
      const recorder = recorderRef.current;
      recorderRef.current = null;
      if (recorder !== null && recorder.state !== 'inactive') {
        // Detach first: this stop is an unmount, not a completion, so nothing must be sent.
        recorder.onstop = null;
        recorder.ondataavailable = null;
        recorder.onerror = null;
        recorder.stop();
      }
      releaseSession();
    };
  }, [maxDurationMs]);

  function onStopClick(): void {
    if (phase === 'starting') {
      // Nothing is being recorded yet; leaving now abandons the pending permission request, and
      // the effect resolves it silently when (and if) the prompt is answered.
      abandonedRef.current = true;
      onCancelRef.current();
      return;
    }
    requestStopRef.current();
  }

  return (
    <div className="recording-bar" role="group" aria-label="Recording a voice note">
      {phase === 'starting' ? (
        <>
          <Spinner />
          <span className="rec-state">Requesting microphone…</span>
        </>
      ) : (
        <>
          <span className="rec-dot" aria-hidden="true" />
          <span className="rec-timer">{formatDuration(elapsedMs)}</span>
          <span className="rec-state">Recording</span>
        </>
      )}
      <button
        type="button"
        className="rec-stop-btn"
        onClick={onStopClick}
        disabled={phase === 'stopping'}
        aria-label={phase === 'starting' ? 'Cancel recording' : 'Stop recording'}
      >
        ■
      </button>
    </div>
  );
}
