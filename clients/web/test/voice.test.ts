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
 *   3. **The content shape and the seal.** The key material the upload sealed under must land in
 *      the message's slots verbatim (they are the only copy the receiver ever gets), the bytes
 *      that cross the wire are the sealed ones — never the recording — and the upload claims the
 *      neutral type for them while the message claims the real container, including the
 *      codec-parameter strip. The server-readable exception keeps the legacy plaintext path, and
 *      the client-side five-minute cap refuses before any bytes cross the wire.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType, MediaKind, sealing } from '@migo/sdk';
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
import {
  LEGACY_PLAINTEXT_SLOTS,
  VOICE_SEAL_DOMAIN,
  isLegacyPlaintext,
} from '../src/lib/migo/media.js';
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

test('a voice note message carries the seal key material in its slots, verbatim', () => {
  const key = new Uint8Array(32).fill(0xab);
  const nonce = new Uint8Array(24).fill(7);
  const content = voiceNoteContent(
    { mediaId: 'media_1' as Id },
    {
      mimeType: 'audio/webm',
      sizeBytes: 4096,
      durationMs: 34_000,
      waveform: new Uint8Array([1, 2]),
    },
    { key, nonce },
  );
  assert.deepEqual(content, {
    type: ContentType.VoiceNoteRef,
    mediaId: 'media_1' as Id,
    mimeType: 'audio/webm',
    sizeBytes: 4096,
    durationMs: 34_000,
    key,
    nonce,
    waveform: new Uint8Array([1, 2]),
  });
});

test('a voice note message omits the waveform it was not given', () => {
  const content = voiceNoteContent(
    { mediaId: 'media_2' as Id },
    { mimeType: 'audio/webm', sizeBytes: 10, durationMs: 500 },
    { key: new Uint8Array(32).fill(1), nonce: new Uint8Array(24) },
  );
  assert.equal(content.type, ContentType.VoiceNoteRef);
  assert.ok(!('waveform' in content), 'an unsampled waveform must stay absent, not zero-filled');
});

test('uploadVoiceNote seals the recording and returns the referencing content', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(client, 'conv_1' as Id, recording());

  assert.equal(calls.length, 1);
  const [options, uploaded] = calls[0] as UploadCall;
  assert.equal(options.kind, MediaKind.VoiceNote);
  assert.equal(
    options.contentType,
    'application/octet-stream',
    'the stored object is ciphertext; the honest claim for it is the neutral one',
  );
  assert.equal(options.conversationId, 'conv_1' as Id);
  assert.equal(options.durationMs, 34_000);
  assert.equal(
    options.size,
    uploaded.length,
    'the upload claims the sealed size, not the recording size',
  );
  assert.notDeepEqual(
    uploaded,
    new Uint8Array([1, 2, 3, 4]),
    'plaintext must never cross the wire',
  );
  assert.equal(uploaded.length, 24 + 4 + 16, 'nonce, ciphertext, and tag');

  // The message describes the content, not the container: the recorded type and the plaintext
  // size, plus the key material the receiver opens with.
  assert.equal(content.type, ContentType.VoiceNoteRef);
  assert.equal(content.mediaId, 'media_1' as Id);
  assert.equal(content.mimeType, 'audio/webm');
  assert.equal(content.sizeBytes, 4);
  assert.equal(content.durationMs, 34_000);
  assert.deepEqual(content.waveform, new Uint8Array([10, 20, 30]));
  assert.equal(content.key.length, 32);
  assert.equal(content.nonce.length, 24);
  assert.ok(!isLegacyPlaintext(content.key), 'a sealed upload never carries the zero marker');

  // The receiver's half, end to end: the message's slots open exactly the recorded bytes.
  assert.deepEqual(
    sealing.open(content.key, content.nonce, VOICE_SEAL_DOMAIN, uploaded),
    new Uint8Array([1, 2, 3, 4]),
  );
});

test('a server-readable destination keeps the legacy plaintext path', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(client, 'conv_room' as Id, recording(), {
    endToEnd: false,
  });

  const [options, uploaded] = calls[0] as UploadCall;
  assert.equal(options.contentType, 'audio/webm', 'a plaintext upload claims its real type');
  assert.equal(options.size, 4);
  assert.deepEqual(
    uploaded,
    new Uint8Array([1, 2, 3, 4]),
    'the legacy path stores the bytes as recorded',
  );
  assert.deepEqual(
    content.key,
    LEGACY_PLAINTEXT_SLOTS.key,
    'the legacy path writes the zero slots',
  );
  assert.deepEqual(content.nonce, LEGACY_PLAINTEXT_SLOTS.nonce);
  assert.equal(content.mimeType, 'audio/webm');
  assert.equal(content.sizeBytes, 4);
});

test('uploadVoiceNote claims the recorded container, not the codec parameters', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(
    client,
    'conv_1' as Id,
    recording({ mimeType: 'audio/webm;codecs=opus' }),
  );
  assert.equal(calls[0]?.[0].contentType, 'application/octet-stream');
  assert.equal(content.mimeType, 'audio/webm');
});

test('uploadVoiceNote substitutes a neutral claim when the browser reported none', async () => {
  const { client, calls } = uploadRecorder();
  const content = await uploadVoiceNote(client, 'conv_1' as Id, recording({ mimeType: '' }));
  assert.equal(calls[0]?.[0].contentType, 'application/octet-stream');
  assert.equal(
    content.mimeType,
    'application/octet-stream',
    'a recording with no container to name claims the neutral type in the message too',
  );
});

test('an over-cap recording is refused client-side before any bytes are uploaded', async () => {
  const { client, calls } = uploadRecorder();
  await assert.rejects(
    uploadVoiceNote(client, 'conv_1' as Id, recording({ durationMs: VOICE_NOTE_MAX_MS + 1 })),
    RangeError,
  );
  assert.equal(calls.length, 0, 'a recording the server would refuse must never begin uploading');
});
