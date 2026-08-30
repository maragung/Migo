/**
 * The voice note helpers: the pure half of record → upload → reference.
 *
 * The browser half — MediaRecorder, the AudioContext sampler, the Audio element — cannot run under
 * Node, so what is pinned here is everything that can:
 *
 *   1. **The waveform fold.** A bar is the max of its slice, the output is always exactly the bar
 *      count, and empty or hostile inputs degrade to silence rather than throwing — the fold runs
 *      over recorded samples and over sender-supplied waveforms alike, so it must never blow up.
 *   2. **The duration format.** `M:SS`, floored, with negative/NaN collapsing to `0:00`.
 *   3. **The content shape.** Placeholder key material (deliberately — the same rule as image
 *      attachments), the exact upload claim including the codec-parameter strip, and the
 *      client-side five-minute cap refusing before any bytes cross the wire.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType, MediaKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import {
  VOICE_NOTE_MAX_MS,
  WAVEFORM_BARS,
  downsampleWaveform,
  formatDuration,
  normalizeVoiceMime,
  pickRecorderMimeType,
  uploadVoiceNote,
  voiceNoteContent,
} from '../src/lib/migo/voice.js';
import type { MediaClient } from '../src/lib/migo/media.js';
import type { VoiceRecording } from '../src/lib/migo/voice.js';

/** A client double that records every upload it is asked to perform. */
type UploadCall = Parameters<MediaClient['media']['upload']>;

function uploadRecorder(): { client: MediaClient; calls: UploadCall[] } {
  const calls: UploadCall[] = [];
  const client: MediaClient = {
    media: {
      upload: (...args: UploadCall) => {
        calls.push(args);
        return Promise.resolve({ mediaId: `media_${calls.length}` as Id });
      },
      download: () => Promise.reject(new Error('download is not under test here')),
    },
  };
  return { client, calls };
}

function recording(overrides: Partial<VoiceRecording> = {}): VoiceRecording {
  return {
    blob: new Blob([new Uint8Array([1, 2, 3, 4])], { type: 'audio/webm' }),
    mimeType: 'audio/webm',
    durationMs: 34_000,
    waveform: new Uint8Array([10, 20, 30]),
    ...overrides,
  };
}

test('formatDuration renders M:SS, floored, with hostile numbers collapsing to zero', () => {
  assert.equal(formatDuration(0), '0:00');
  assert.equal(formatDuration(999), '0:00');
  assert.equal(formatDuration(1_000), '0:01');
  assert.equal(formatDuration(34_000), '0:34');
  assert.equal(formatDuration(59_999), '0:59');
  assert.equal(formatDuration(60_000), '1:00');
  assert.equal(formatDuration(754_000), '12:34');
  assert.equal(formatDuration(VOICE_NOTE_MAX_MS), '5:00');
  // A negative or non-finite input must not print `-0:01` or `NaN:NaN`.
  assert.equal(formatDuration(-2_500), '0:00');
  assert.equal(formatDuration(Number.NaN), '0:00');
  assert.equal(formatDuration(Number.POSITIVE_INFINITY), '0:00');
});

test('downsampleWaveform takes the max of each bar slice', () => {
  assert.deepEqual(
    downsampleWaveform([0, 10, 20, 30], 2),
    new Uint8Array([10, 30]),
    'each bar is the peak of its half, not an average of it',
  );
  assert.deepEqual(
    downsampleWaveform([0, 0, 100, 0, 0, 100, 0, 0, 0, 0], 5),
    new Uint8Array([0, 100, 100, 0, 0]),
    'a syllable inside a bucket must survive the fold',
  );
});

test('downsampleWaveform always returns exactly the bar count', () => {
  assert.equal(downsampleWaveform([], WAVEFORM_BARS).length, WAVEFORM_BARS);
  assert.deepEqual(
    downsampleWaveform([], WAVEFORM_BARS),
    new Uint8Array(WAVEFORM_BARS),
    'no samples is all silence, not an error',
  );
  // Fewer samples than bars: the samples lead and the tail pads with silence.
  assert.deepEqual(downsampleWaveform([7, 8, 9], 5), new Uint8Array([7, 8, 9, 0, 0]));
});

test('downsampleWaveform clamps hostile sample values into the 0-255 byte', () => {
  assert.deepEqual(downsampleWaveform([300, -5, Number.NaN], 3), new Uint8Array([255, 0, 0]));
});

test('normalizeVoiceMime strips codec parameters to the container claim', () => {
  assert.equal(normalizeVoiceMime('audio/webm;codecs=opus'), 'audio/webm');
  assert.equal(normalizeVoiceMime('audio/mp4'), 'audio/mp4');
  assert.equal(normalizeVoiceMime(''), '');
});

test('pickRecorderMimeType prefers webm and defers to the browser without it', () => {
  // Under Node there is no MediaRecorder at all, which is itself the interesting case: the
  // preference must degrade to "browser, you choose" (the empty string), never throw.
  assert.equal(pickRecorderMimeType(), '');
});

test('a voice note message references the uploaded object with placeholder key material', () => {
  const content = voiceNoteContent(
    { mediaId: 'media_1' as Id },
    {
      mimeType: 'audio/webm',
      sizeBytes: 4096,
      durationMs: 34_000,
      waveform: new Uint8Array([1, 2]),
    },
  );
  assert.deepEqual(content, {
    type: ContentType.VoiceNoteRef,
    mediaId: 'media_1' as Id,
    mimeType: 'audio/webm',
    sizeBytes: 4096,
    durationMs: 34_000,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    waveform: new Uint8Array([1, 2]),
  });
});

test('a voice note message omits the waveform it was not given', () => {
  const content = voiceNoteContent(
    { mediaId: 'media_2' as Id },
    { mimeType: 'audio/webm', sizeBytes: 10, durationMs: 500 },
  );
  assert.equal(content.type, ContentType.VoiceNoteRef);
  assert.ok(!('waveform' in content), 'an unsampled waveform must stay absent, not zero-filled');
});

test('uploadVoiceNote uploads as a voice note and returns the referencing content', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(client, 'conv_1' as Id, recording());

  assert.equal(calls.length, 1);
  const [options, bytes] = calls[0] as UploadCall;
  assert.equal(options.kind, MediaKind.VoiceNote);
  assert.equal(options.contentType, 'audio/webm');
  assert.equal(options.size, 4);
  assert.equal(options.conversationId, 'conv_1' as Id);
  assert.equal(options.durationMs, 34_000);
  assert.deepEqual(bytes, new Uint8Array([1, 2, 3, 4]));

  assert.equal(content.type, ContentType.VoiceNoteRef);
  assert.equal(content.mediaId, 'media_1' as Id);
  assert.equal(content.mimeType, 'audio/webm');
  assert.equal(content.sizeBytes, 4);
  assert.equal(content.durationMs, 34_000);
  assert.deepEqual(content.waveform, new Uint8Array([10, 20, 30]));
  assert.deepEqual(content.key, new Uint8Array(32));
  assert.deepEqual(content.nonce, new Uint8Array(12));
});

test('uploadVoiceNote claims the recorded container, not the codec parameters', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(
    client,
    'conv_1' as Id,
    recording({ mimeType: 'audio/webm;codecs=opus' }),
  );
  assert.equal(calls[0]?.[0].contentType, 'audio/webm');
  assert.equal(content.mimeType, 'audio/webm');
});

test('uploadVoiceNote substitutes a neutral claim when the browser reported none', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(client, 'conv_1' as Id, recording({ mimeType: '' }));
  assert.equal(calls[0]?.[0].contentType, 'application/octet-stream');
  assert.equal(content.mimeType, 'application/octet-stream');
});

test('an over-cap recording is refused client-side before any bytes are uploaded', async () => {
  const { client, calls } = uploadRecorder();
  await assert.rejects(
    uploadVoiceNote(client, 'conv_1' as Id, recording({ durationMs: VOICE_NOTE_MAX_MS + 1 })),
    RangeError,
  );
  assert.equal(calls.length, 0, 'a recording the server would refuse must never begin uploading');
});
