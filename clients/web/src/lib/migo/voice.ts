'use client';

/**
 * Voice notes in the web client: the pure half of record → upload → reference.
 *
 * The split mirrors `lib/migo/media.ts` (the image attachment path). An upload is one convenience
 * call — {@link uploadVoiceNote} hides begin/PUT/commit — and the message body that references the
 * object is built by a pure function, {@link voiceNoteContent}, a test can pin. The browser half
 * (MediaRecorder, the AudioContext sampler, the `Audio` element) cannot run under Node, so it stays
 * in the components, which call these helpers with the results; everything testable about a voice
 * note — the waveform fold, the duration format, the upload claim, the message shape — lives here.
 *
 * # The placeholder key material
 *
 * Same rule as image attachments: a `VoiceNoteRefContent` carries the symmetric `key` and `nonce`
 * that will open the object once media sealing lands. Until then the bytes are stored as uploaded,
 * the player downloads by `mediaId` without decrypting, and the slots carry zero-filled placeholder
 * material so the message shape is already the final one. Swapping the placeholders for real key
 * material later touches nothing else here.
 *
 * # The five-minute cap
 *
 * The server refuses a voice note longer than 300,000 ms. The recorder enforces the cap while
 * recording — it stops at the deadline rather than letting a user record minutes the server would
 * then reject — and {@link uploadVoiceNote} refuses an over-cap recording before any bytes cross
 * the wire, so no other caller can regress the rule.
 */

import { ContentType, MediaKind } from '@migo/sdk';
import type { Id, UploadResult, VoiceNoteRefContent } from '@migo/sdk';

import type { MediaClient } from './media.js';

/** The server's cap on a voice note; a longer recording is refused at upload. */
export const VOICE_NOTE_MAX_MS = 300_000;

/** How many bars a recorded waveform is folded into, and what a bubble renders at most. */
export const WAVEFORM_BARS = 50;

/**
 * One finished recording, as the recorder hands it to the uploader.
 *
 * `mimeType` is what the browser actually produced (normalised, no codec parameters), never the
 * type it was merely asked for; `waveform` is optional because the analyser graph is best-effort —
 * a recording that never sampled amplitude still uploads, and the player falls back to a progress
 * bar. The blob itself is never given an object URL: its bytes go straight to `arrayBuffer()`.
 */
export interface VoiceRecording {
  blob: Blob;
  /** The recorded container type, e.g. `audio/webm`; empty when the browser reports none. */
  mimeType: string;
  durationMs: number;
  /** ~{@link WAVEFORM_BARS} amplitude bytes, 0–255, or absent when none were sampled. */
  waveform?: Uint8Array;
}

/**
 * A recording or playback length as `M:SS` — `0:34`, `1:00`, `62:04` — the shape both the
 * recording timer and a bubble's duration label use.
 *
 * Rounds down (a 59.9-second note reads `0:59`, matching every player's floor convention) and
 * collapses a negative or non-finite input to `0:00` rather than printing `-0:01` or `NaN:NaN`.
 * Minutes are not rollover-bounded: a cap of five minutes fits comfortably, and `75:00` is still
 * an honest `M:SS`.
 */
export function formatDuration(durationMs: number): string {
  const safe = Number.isFinite(durationMs) && durationMs > 0 ? durationMs : 0;
  const totalSeconds = Math.floor(safe / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

/**
 * Folds a stream of amplitude samples into the fixed-width bar chart a bubble renders.
 *
 * Each output bar is the *maximum* sample in its slice of the input, because a peak — not an
 * average — is what a waveform bar is drawn from: a syllable landing inside a bucket must show,
 * and averaging would flatten it into the silence around it. The output is always exactly
 * `barCount` bytes: an input shorter than the bar count pads with silence at the tail (a very
 * short recording simply runs out of samples), an empty input is all silence, and values are
 * clamped to the 0–255 byte the `waveform` field carries. The fold also runs over sender-supplied
 * waveforms at render time, so it must degrade gracefully on hostile input, not throw.
 */
export function downsampleWaveform(
  samples: ArrayLike<number>,
  barCount = WAVEFORM_BARS,
): Uint8Array {
  const bars = new Uint8Array(barCount);
  if (samples.length === 0) {
    return bars;
  }
  const bucketSize = Math.max(1, Math.ceil(samples.length / barCount));
  for (let i = 0; i < samples.length; i += 1) {
    const value = samples[i] ?? 0;
    const clamped = Number.isFinite(value) ? Math.max(0, Math.min(255, Math.round(value))) : 0;
    const barIndex = Math.min(barCount - 1, Math.floor(i / bucketSize));
    if (clamped > (bars[barIndex] ?? 0)) {
      bars[barIndex] = clamped;
    }
  }
  return bars;
}

/**
 * The MIME type to ask `MediaRecorder` for: `audio/webm` when the browser can produce it, and the
 * empty string — "browser, you choose" — when it cannot (Safari records `audio/mp4`).
 *
 * The *claim* must describe what was actually recorded, so the recorder reports the type off the
 * finished blob rather than off this preference; this only picks the preferred container.
 */
export function pickRecorderMimeType(): string {
  if (typeof MediaRecorder === 'undefined') {
    return '';
  }
  return MediaRecorder.isTypeSupported('audio/webm') ? 'audio/webm' : '';
}

/**
 * Normalises a recorded MIME type to its container claim: the type before any `;` parameters.
 *
 * `MediaRecorder.mimeType` reports e.g. `audio/webm;codecs=opus` — the parameter is true but noise
 * in a claim the server matches against the bytes at commit, and section 122 says receivers must
 * not act on the claim anyway. An empty input stays empty; {@link uploadVoiceNote} substitutes the
 * neutral claim image attachments use.
 */
export function normalizeVoiceMime(mimeType: string): string {
  return (mimeType.split(';', 1)[0] ?? '').trim();
}

/**
 * Placeholder key material for the media key slots; see the module doc.
 *
 * Zero-filled and of the lengths the future encryption will use, so the message shape is final.
 */
const PLACEHOLDER_VOICE_KEY = new Uint8Array(32);
const PLACEHOLDER_VOICE_NONCE = new Uint8Array(12);

/**
 * The message body for an uploaded voice note: the reference the receiver renders, in the sender's
 * claim of type and duration, with the placeholder key material (see the module doc).
 *
 * Extracted from {@link uploadVoiceNote} so the content shape is a pure function a test can pin —
 * the placeholder rule especially, since "these bytes are not really encrypted yet" is a fact a
 * future change must replace deliberately, not drift away from.
 */
export function voiceNoteContent(
  uploaded: UploadResult,
  claim: {
    mimeType: string;
    sizeBytes: number;
    durationMs: number;
    waveform?: Uint8Array;
  },
): VoiceNoteRefContent {
  const content: VoiceNoteRefContent = {
    type: ContentType.VoiceNoteRef,
    mediaId: uploaded.mediaId,
    mimeType: claim.mimeType,
    sizeBytes: claim.sizeBytes,
    durationMs: claim.durationMs,
    key: PLACEHOLDER_VOICE_KEY,
    nonce: PLACEHOLDER_VOICE_NONCE,
  };
  if (claim.waveform !== undefined) {
    content.waveform = claim.waveform;
  }
  return content;
}

/**
 * Uploads a finished recording into a conversation and returns the message body that references it.
 *
 * The client-side five-minute cap is enforced here as well as in the recorder: an over-cap
 * recording is refused before any bytes cross the wire (the server would refuse it at commit
 * anyway, and a failed five-minute upload is the worst possible place to learn that). The
 * recording's duration and waveform ride both the upload and the message, so the receiver can lay
 * out the player before downloading anything.
 */
export async function uploadVoiceNote(
  client: MediaClient,
  conversationId: Id,
  recording: VoiceRecording,
): Promise<VoiceNoteRefContent> {
  if (recording.durationMs > VOICE_NOTE_MAX_MS) {
    throw new RangeError(`voice notes are capped at ${VOICE_NOTE_MAX_MS} ms`);
  }
  const container = normalizeVoiceMime(recording.mimeType);
  const claim = container === '' ? 'application/octet-stream' : container;
  const bytes = new Uint8Array(await recording.blob.arrayBuffer());
  const uploaded = await client.media.upload(
    {
      kind: MediaKind.VoiceNote,
      contentType: claim,
      size: bytes.length,
      conversationId,
      durationMs: recording.durationMs,
    },
    bytes,
  );
  return voiceNoteContent(uploaded, {
    mimeType: claim,
    sizeBytes: bytes.length,
    durationMs: recording.durationMs,
    ...(recording.waveform !== undefined ? { waveform: recording.waveform } : {}),
  });
}
