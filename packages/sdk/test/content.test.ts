/**
 * The inner plaintext round-trips every content type, and padding collapses length into fixed buckets.
 *
 * Everything here lives inside the ciphertext, so it is a client-to-client contract the server never
 * sees. Two properties carry weight. The `content_type || body` codec must round-trip exactly, and an
 * unknown type byte from a newer client must surface as a clean "unsupported" error rather than crash
 * the conversation or, worse, mis-decode into a valid-looking wrong body. And the padding must do its
 * one job: ciphertext length leaks through the envelope even when content does not, so "yes" and "no"
 * must seal to the same number of bytes. If padding silently regressed — rounded to the wrong bucket,
 * or left the length correlated with the plaintext — short replies would again be distinguishable by
 * length to anyone counting bytes on the wire, and every functional test would still pass. So this
 * file pins the round trip for all five types, the exact bucket boundaries, the length-equalising
 * property, that padding is unread zero bytes, and the decoder's rejection of empty and unknown input.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ContentType,
  SdkError,
  conversationContext,
  decodeContent,
  encodeContent,
} from '../src/index.js';
import type { MessageContent } from '../src/index.js';
import { idFromBytes, idToBytes } from '@migo/wire';
import { idOf } from './harness.js';

/** The buckets the source rounds up to; mirrored here so the boundary test is independent of it. */
const BUCKETS = [64, 256, 1024, 4096, 16384];

/** The padded length the codec should choose for an unpadded plaintext of `length` bytes. */
function expectedBucket(length: number): number {
  for (const bucket of BUCKETS) {
    if (length <= bucket) {
      return bucket;
    }
  }
  const largest = BUCKETS[BUCKETS.length - 1] ?? 16384;
  return Math.ceil(length / largest) * largest;
}

/** One representative value of every content type, with its optional fields populated. */
const SAMPLES: MessageContent[] = [
  { type: ContentType.Text, text: 'hello world', mentions: [idOf(7), idOf(8)] },
  {
    type: ContentType.MediaRef,
    mediaId: idOf(2),
    mimeType: 'image/png',
    sizeBytes: 12_345,
    key: new Uint8Array([1, 2, 3, 4]),
    nonce: new Uint8Array([5, 6, 7, 8]),
    width: 640,
    height: 480,
    blurhash: 'LKO2',
    caption: 'a photo',
  },
  {
    type: ContentType.VoiceNoteRef,
    mediaId: idOf(3),
    mimeType: 'audio/opus',
    sizeBytes: 9_000,
    durationMs: 4_200,
    key: new Uint8Array([9, 9, 9]),
    nonce: new Uint8Array([8, 8, 8]),
    waveform: new Uint8Array([10, 20, 30, 40]),
  },
  { type: ContentType.Reaction, targetMessageId: idOf(4), emoji: '👍', remove: false },
  { type: ContentType.ControlEvent, event: 'sender-key', data: new Uint8Array([0xde, 0xad]) },
];

test('every content type round-trips through encode and decode unchanged', () => {
  for (const sample of SAMPLES) {
    const decoded = decodeContent(encodeContent(sample));
    assert.deepEqual(decoded, sample, `content type ${sample.type} did not round-trip`);
  }
});

test('optional fields left off stay off after a round trip, rather than reappearing as defaults', () => {
  // A Text with no mentions must decode to a body with no mentions key at all — not an empty array,
  // which would be a different value a caller might branch on.
  const bare: MessageContent = { type: ContentType.Text, text: 'no mentions' };
  assert.deepEqual(decodeContent(encodeContent(bare)), bare);

  const control: MessageContent = { type: ContentType.ControlEvent, event: 'ping' };
  assert.deepEqual(decodeContent(encodeContent(control)), control);
});

test('padding rounds the plaintext up to the correct fixed bucket at every size', () => {
  // Span every bucket and past the top of the table, where lengths round to a multiple of the
  // largest bucket. The unpadded length is measured with padding off, so the boundary is exact.
  for (const length of [0, 100, 500, 2_000, 5_000, 20_000]) {
    const content: MessageContent = { type: ContentType.Text, text: 'x'.repeat(length) };
    const unpadded = encodeContent(content, { pad: false }).length;
    const padded = encodeContent(content).length;
    assert.equal(padded, expectedBucket(unpadded), `wrong bucket for unpadded length ${unpadded}`);
    assert.ok(padded >= unpadded, 'padding produced a buffer shorter than the plaintext');
  }
});

test('two short messages of different content seal to the same length', () => {
  // The whole point of padding: an observer counting bytes cannot tell a "yes" from a "no".
  const yes = encodeContent({ type: ContentType.Text, text: 'yes' });
  const no = encodeContent({ type: ContentType.Text, text: 'no' });
  const other = encodeContent({ type: ContentType.Text, text: 'maybe' });
  assert.equal(yes.length, no.length);
  assert.equal(yes.length, other.length);
  assert.equal(yes.length, 64, 'short messages should land in the smallest bucket');

  // And without padding the lengths differ — proving the equality above is padding at work, not a
  // coincidence of these particular strings.
  const yesRaw = encodeContent({ type: ContentType.Text, text: 'yes' }, { pad: false });
  const noRaw = encodeContent({ type: ContentType.Text, text: 'no' }, { pad: false });
  assert.notEqual(yesRaw.length, noRaw.length);
});

test('padding is trailing zero bytes and is ignored on decode', () => {
  const content: MessageContent = { type: ContentType.Text, text: 'padded' };
  const unpadded = encodeContent(content, { pad: false });
  const padded = encodeContent(content);
  assert.ok(padded.length > unpadded.length, 'this sample was expected to gain padding');
  // Everything past the real plaintext is zero: no extra entropy that could hint at the boundary.
  for (let i = unpadded.length; i < padded.length; i += 1) {
    assert.equal(padded[i], 0, `padding byte at ${i} was not zero`);
  }
  // The padding changes nothing about the decoded value.
  assert.deepEqual(decodeContent(padded), content);
});

test('an empty plaintext is rejected rather than decoded', () => {
  assert.throws(() => decodeContent(new Uint8Array([])), SdkError);
});

test('an unknown content type is reported as unsupported, naming the offending byte', () => {
  // A message from a newer client: the decoder must not guess a struct for byte 99, and must say so.
  assert.throws(
    () => decodeContent(new Uint8Array([99])),
    (error: unknown) => {
      assert.ok(error instanceof SdkError);
      assert.match(error.message, /99/);
      return true;
    },
  );
});

test('a known type with a truncated body fails instead of decoding a partial struct', () => {
  // The type byte says Text, but there is no MSE body behind it; the reader must throw.
  assert.throws(() => decodeContent(new Uint8Array([ContentType.Text])));
});

test('the conversation context is exactly the 16 identifier bytes, and round-trips', () => {
  const conversationId = idOf(123);
  const context = conversationContext(conversationId);
  assert.deepEqual(context, idToBytes(conversationId));
  assert.equal(context.length, 16);
  assert.equal(idFromBytes(context), conversationId);
});
