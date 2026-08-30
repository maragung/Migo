/**
 * The media helpers: what an uploaded image claims, and how long a resolved URL is good for.
 *
 * Two rules carry correctness weight and would silently regress under a "helpful" refactor, so
 * they are pinned here as pure functions over a client double (the {@link MediaClient} slice the
 * helpers actually use, which is why no whole-client mock is needed):
 *
 *   1. **A media message's key slots are placeholder material, deliberately.** Sealing media before
 *      upload is a future feature; until it lands the bytes are stored as uploaded and the renderer
 *      downloads by `mediaId` without decrypting. The content shape — including the zero-filled key
 *      and nonce — is the final contract, so a change here must be a deliberate swap to real key
 *      material, never a drift.
 *   2. **A resolved URL is cached per media id until it is near expiry.** A signed URL outlives any
 *      one conversation view, so re-renders refetch nothing — but a URL past its deadline is
 *      refetched, because an expired grant serves nothing. In-flight requests are shared, so a
 *      conversation full of the same image resolves with one download.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { imageAttachmentContent, readFileBytes, resolveMediaUrl } from '../src/lib/migo/media.js';
import type { MediaClient } from '../src/lib/migo/media.js';

/** A client double whose `media.download` hands out URLs and counts how often it was asked. */
function downloadCounter(urls: Array<{ url: string; expiresAt: number }>): {
  client: MediaClient;
  downloads: Id[];
} {
  const downloads: Id[] = [];
  const client: MediaClient = {
    media: {
      // The upload half is unused by these tests; present only to satisfy the slice.
      upload: () => Promise.reject(new Error('upload is not under test here')),
      download: (objectId: Id) => {
        downloads.push(objectId);
        const granted = urls[downloads.length - 1];
        assert.ok(granted !== undefined, 'the double ran out of canned URLs');
        return Promise.resolve(granted);
      },
    },
  };
  return { client, downloads };
}

test('a media message references the uploaded object with placeholder key material', () => {
  const content = imageAttachmentContent(
    { mediaId: 'media_1' as Id },
    { mimeType: 'image/png', sizeBytes: 4096, width: 640, height: 480 },
  );
  assert.deepEqual(content, {
    type: ContentType.MediaRef,
    mediaId: 'media_1' as Id,
    mimeType: 'image/png',
    sizeBytes: 4096,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    width: 640,
    height: 480,
  });
});

test('a media message omits the dimensions it was not given', () => {
  const content = imageAttachmentContent(
    { mediaId: 'media_2' as Id },
    { mimeType: 'image/jpeg', sizeBytes: 10 },
  );
  assert.equal(content.type, ContentType.MediaRef);
  assert.equal(content.mediaId, 'media_2' as Id);
  assert.ok(!('width' in content), 'an unknown width must stay absent, not zero');
  assert.ok(!('height' in content), 'an unknown height must stay absent, not zero');
});

test('readFileBytes returns exactly the file bytes', async () => {
  const bytes = new Uint8Array([1, 2, 3, 4, 5]);
  const file = new File([bytes], 'pic.png', { type: 'image/png' });
  assert.deepEqual(await readFileBytes(file), bytes);
});

test('a resolved URL is cached per media id, so repeat renders never refetch', async () => {
  const { client, downloads } = downloadCounter([
    { url: 'https://media.example.test/a', expiresAt: Date.now() + 3_600_000 },
  ]);
  const id = 'media_cache' as Id;
  assert.equal(await resolveMediaUrl(client, id), 'https://media.example.test/a');
  assert.equal(await resolveMediaUrl(client, id), 'https://media.example.test/a');
  assert.equal(downloads.length, 1, 'the second resolve must be served from the cache');
});

test('concurrent resolves for one media id share a single download', async () => {
  const { client, downloads } = downloadCounter([
    { url: 'https://media.example.test/b', expiresAt: Date.now() + 3_600_000 },
  ]);
  const id = 'media_inflight' as Id;
  const [first, second] = await Promise.all([
    resolveMediaUrl(client, id),
    resolveMediaUrl(client, id),
  ]);
  assert.equal(first, 'https://media.example.test/b');
  assert.equal(second, 'https://media.example.test/b');
  assert.equal(downloads.length, 1, 'two concurrent resolves must not race two downloads');
});

test('a URL past its deadline is refetched, not served stale', async () => {
  // The double hands each download an already-expired grant, so every resolve must go back.
  const { client, downloads } = downloadCounter([
    { url: 'https://media.example.test/old1', expiresAt: Date.now() - 1_000 },
    { url: 'https://media.example.test/old2', expiresAt: Date.now() - 1_000 },
  ]);
  const id = 'media_expired' as Id;
  assert.equal(await resolveMediaUrl(client, id), 'https://media.example.test/old1');
  assert.equal(await resolveMediaUrl(client, id), 'https://media.example.test/old2');
  assert.equal(downloads.length, 2, 'an expired URL must be replaced by a fresh one');
});

test('a failed download is not cached, so the next render retries', async () => {
  let attempts = 0;
  const client: MediaClient = {
    media: {
      upload: () => Promise.reject(new Error('upload is not under test here')),
      download: (objectId: Id) => {
        attempts += 1;
        if (attempts === 1) {
          return Promise.reject(new Error('media unavailable'));
        }
        return Promise.resolve({
          url: `https://media.example.test/${String(objectId)}`,
          expiresAt: Date.now() + 3_600_000,
        });
      },
    },
  };
  const id = 'media_retry' as Id;
  await assert.rejects(resolveMediaUrl(client, id));
  assert.equal(await resolveMediaUrl(client, id), 'https://media.example.test/media_retry');
  assert.equal(attempts, 2);
});
