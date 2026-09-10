/**
 * The media helpers: what a sealed upload claims, and how long a resolved URL is good for.
 *
 * The rules that carry correctness weight and would silently regress under a "helpful" refactor,
 * pinned here as pure functions and client doubles (the {@link MediaClient} slice the helpers
 * actually use, which is why no whole-client mock is needed):
 *
 *   1. **A media message's key slots carry the seal's own material, verbatim.** The upload seals
 *      the bytes under a fresh key; the content's `key`/`nonce` slots are the only copies that
 *      ever reach the receiver, so what the upload sealed under must be exactly what the message
 *      carries. The legacy plaintext path is the one writer of zero material, and the renderer's
 *      zero-key detection is what keeps every pre-sealing message rendering.
 *   2. **A sealed upload stores ciphertext and claims it honestly.** The bytes that cross the wire
 *      are not the file's bytes, the claimed type is the neutral one, and the *message* still
 *      carries the sender's claim of type and plaintext size — the claim is a label for the
 *      decrypted object, not a fact about what is stored.
 *   3. **A resolved URL is cached per media id until it is near expiry.** A signed URL outlives any
 *      one conversation view, so re-renders refetch nothing — but a URL past its deadline is
 *      refetched, because an expired grant serves nothing. In-flight requests are shared, so a
 *      conversation full of the same image resolves with one download.
 *   4. **A resolved object is fetched, opened, and cached per media id.** The renderer never
 *      touches a URL directly: the object helper fetches the bytes, opens them with the message's
 *      key and nonce (or passes legacy plaintext through), and hands back a blob URL typed with
 *      the sender's claim. A failure is not cached, because a failed download or a failed open
 *      says nothing durable about the object.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ContentType, MediaKind, sealing } from '@migo/sdk';
import type { Id, MediaRefContent } from '@migo/sdk';

import {
  DOCUMENT_MAX_BYTES,
  LEGACY_PLAINTEXT_SLOTS,
  documentAttachmentContent,
  imageAttachmentContent,
  isLegacyPlaintext,
  readFileBytes,
  resolveMediaObject,
  resolveMediaUrl,
  uploadDocumentAttachment,
  uploadImageAttachment,
} from '../src/lib/migo/media.js';
import type { MediaClient } from '../src/lib/migo/media.js';

/**
 * Node has no `Image` global, and the upload path probes the file's dimensions with one. The stub
 * always fails to load — `imageDimensions`' honest null path, the one a browser takes for a file
 * that will not decode — so the upload tests exercise the no-dimensions branch under Node.
 */
class UnloadableImage {
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  set src(_url: string) {
    this.onerror?.();
  }
}
Reflect.set(globalThis, 'Image', UnloadableImage);

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

/**
 * A client double that records every upload it is asked to perform, so a test can pin both the
 * upload claim and the bytes that actually crossed the wire.
 */
type UploadCall = Parameters<MediaClient['media']['upload']>;

function uploadRecorder(): { client: MediaClient; calls: UploadCall[] } {
  const calls: UploadCall[] = [];
  const client: MediaClient = {
    media: {
      upload: (...args: UploadCall) => {
        calls.push(args);
        return Promise.resolve({ mediaId: `media_${calls.length}` as Id });
      },
      download: (objectId: Id) =>
        Promise.resolve({
          url: `https://media.example.test/${String(objectId)}`,
          expiresAt: Date.now() + 3_600_000,
        }),
    },
  };
  return { client, calls };
}

/** A PNG-flavoured file of `bytes`, the shape a picker hands the upload helper. */
function imageFile(bytes: Uint8Array): File {
  return new File([bytes.slice()], 'pic.png', { type: 'image/png' });
}

// --- the content shape and its key slots ---

test('a media message carries the seal key material in its slots, verbatim', () => {
  const key = generateKey();
  const nonce = new Uint8Array(24).fill(7);
  const content = imageAttachmentContent(
    { mediaId: 'media_1' as Id },
    { mimeType: 'image/png', sizeBytes: 4_096, width: 640, height: 480 },
    { key, nonce },
  );
  assert.deepEqual(content, {
    type: ContentType.MediaRef,
    mediaId: 'media_1' as Id,
    mimeType: 'image/png',
    sizeBytes: 4_096,
    key,
    nonce,
    width: 640,
    height: 480,
  });
});

test('a media message omits the dimensions it was not given', () => {
  const content = imageAttachmentContent(
    { mediaId: 'media_2' as Id },
    { mimeType: 'image/jpeg', sizeBytes: 10 },
    { key: generateKey(), nonce: new Uint8Array(24) },
  );
  assert.equal(content.type, ContentType.MediaRef);
  assert.equal(content.mediaId, 'media_2' as Id);
  assert.ok(!('width' in content), 'an unknown width must stay absent, not zero');
  assert.ok(!('height' in content), 'an unknown height must stay absent, not zero');
});

test('the legacy plaintext slots are zero, and only an all-zero key is legacy', () => {
  assert.deepEqual(LEGACY_PLAINTEXT_SLOTS.key, new Uint8Array(32));
  assert.deepEqual(LEGACY_PLAINTEXT_SLOTS.nonce, new Uint8Array(24));
  assert.ok(isLegacyPlaintext(new Uint8Array(32)), 'the zero slot is the legacy marker');
  assert.ok(isLegacyPlaintext(LEGACY_PLAINTEXT_SLOTS.key));
  assert.ok(
    !isLegacyPlaintext(generateKey()),
    'a sealed upload key is 256 random bits, never all zeros',
  );
});

test('readFileBytes returns exactly the file bytes', async () => {
  const bytes = new Uint8Array([1, 2, 3, 4, 5]);
  const file = new File([bytes], 'pic.png', { type: 'image/png' });
  assert.deepEqual(await readFileBytes(file), bytes);
});

/** A 32-byte key with set bits, so no test key can ever be the all-zero legacy marker. */
function generateKey(): Uint8Array {
  return new Uint8Array(32).fill(0xab);
}

// --- the sealed upload ---

test('an end-to-end upload seals the bytes and carries the key inside the message', async () => {
  const { client, calls } = uploadRecorder();
  const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
  const content = await uploadImageAttachment(client, 'conv_1' as Id, imageFile(bytes));

  assert.equal(calls.length, 1);
  const [options, uploaded] = calls[0] as UploadCall;
  assert.equal(options.kind, MediaKind.Image);
  assert.equal(options.conversationId, 'conv_1' as Id);
  assert.equal(
    options.contentType,
    'application/octet-stream',
    'the stored object is ciphertext; the honest claim for it is the neutral one',
  );
  assert.equal(
    options.size,
    uploaded.length,
    'the upload claims the sealed size, not the plaintext size',
  );
  assert.notDeepEqual(uploaded, bytes, 'plaintext must never cross the wire');
  assert.equal(uploaded.length, 24 + bytes.length + 16, 'nonce, ciphertext, and tag');

  // The message describes the content, not the container: the sender's real mime claim, the
  // plaintext size, and the key material the receiver opens with.
  assert.equal(content.mimeType, 'image/png');
  assert.equal(content.sizeBytes, bytes.length);
  assert.equal(content.key.length, 32);
  assert.equal(content.nonce.length, 24);
  assert.ok(!isLegacyPlaintext(content.key));

  // The receiver's half, end to end: what the message's slots open must be exactly the file.
  assert.deepEqual(
    sealing.open(content.key, content.nonce, new TextEncoder().encode('migo-media'), uploaded),
    bytes,
  );
});

test('a server-readable destination keeps the legacy plaintext path', async () => {
  const { client, calls } = uploadRecorder();
  const bytes = new Uint8Array([9, 8, 7, 6]);
  const content = await uploadImageAttachment(client, 'conv_room' as Id, imageFile(bytes), {
    endToEnd: false,
  });

  const [options, uploaded] = calls[0] as UploadCall;
  assert.equal(options.contentType, 'image/png', 'a plaintext upload claims its real type');
  assert.equal(options.size, bytes.length);
  assert.deepEqual(uploaded, bytes, 'the legacy path stores the bytes as uploaded');
  assert.deepEqual(
    content.key,
    LEGACY_PLAINTEXT_SLOTS.key,
    'the legacy path writes the zero slots',
  );
  assert.deepEqual(content.nonce, LEGACY_PLAINTEXT_SLOTS.nonce);
  assert.equal(content.sizeBytes, bytes.length);
});

// --- the document upload ---

/** A PDF-flavoured file of `bytes`, the shape a picker hands the document helper. */
function documentFile(bytes: Uint8Array, name = 'report.pdf'): File {
  return new File([bytes.slice()], name, { type: 'application/pdf' });
}

test('a document upload content carries the filename, mime, and plaintext size', () => {
  const key = generateKey();
  const nonce = new Uint8Array(24).fill(3);
  const content = documentAttachmentContent(
    { mediaId: 'media_doc' as Id },
    { mimeType: 'application/pdf', sizeBytes: 2_048, fileName: 'report.pdf' },
    { key, nonce },
  );
  assert.deepEqual(content, {
    type: ContentType.MediaRef,
    mediaId: 'media_doc' as Id,
    mimeType: 'application/pdf',
    sizeBytes: 2_048,
    key,
    nonce,
    caption: 'report.pdf',
  });
});

test('a document upload seals the bytes, claims Document, and rides the media seal domain', async () => {
  const { client, calls } = uploadRecorder();
  const bytes = new Uint8Array([9, 9, 9, 9]);
  const content = await uploadDocumentAttachment(client, 'conv_1' as Id, documentFile(bytes));

  assert.equal(calls.length, 1);
  const [options, uploaded] = calls[0] as UploadCall;
  assert.equal(options.kind, MediaKind.Document);
  assert.equal(options.conversationId, 'conv_1' as Id);
  assert.equal(
    options.contentType,
    'application/octet-stream',
    'the stored object is ciphertext; the honest claim for it is the neutral one',
  );
  assert.equal(options.size, uploaded.length, 'the upload claims the sealed size');
  assert.notDeepEqual(uploaded, bytes, 'plaintext must never cross the wire');

  // The message carries the file's name in the caption slot, the real mime claim, and the
  // plaintext size — and the slots open back to exactly the file, under the media domain.
  assert.equal(content.caption, 'report.pdf');
  assert.equal(content.mimeType, 'application/pdf');
  assert.equal(content.sizeBytes, bytes.length);
  assert.ok(!isLegacyPlaintext(content.key));
  assert.deepEqual(
    sealing.open(content.key, content.nonce, new TextEncoder().encode('migo-media'), uploaded),
    bytes,
  );
});

test('a file with no browser-known type claims the neutral mime', async () => {
  const { client, calls } = uploadRecorder();
  const bytes = new Uint8Array([1, 2]);
  const file = new File([bytes.slice()], 'payload.bin', { type: '' });
  const content = await uploadDocumentAttachment(client, 'conv_1' as Id, file);
  assert.equal(content.mimeType, 'application/octet-stream');
  assert.equal(content.caption, 'payload.bin');
  assert.equal((calls[0] as UploadCall)[0]?.kind, MediaKind.Document);
});

test('a document over the ceiling rejects before any upload call', async () => {
  const { client, calls } = uploadRecorder();
  const huge = new Uint8Array(DOCUMENT_MAX_BYTES + 1);
  const file = new File([huge.slice()], 'too-big.pdf', { type: 'application/pdf' });
  await assert.rejects(uploadDocumentAttachment(client, 'conv_1' as Id, file), RangeError);
  assert.equal(calls.length, 0, 'an over-ceiling file must never reach the upload');
});

// --- the resolved URL cache ---

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

// --- the resolved object: fetch, open, cache ---

/**
 * Substitutes a fetch that answers the canned bodies in order, counting the URLs it was asked
 * for. `resolveMediaObject` reads `globalThis.fetch` lazily, so a double installed before the
 * call is the one it uses.
 */
function fetchRecorder(bodies: Array<Uint8Array | Error>): {
  fetch: typeof globalThis.fetch;
  urls: string[];
} {
  const urls: string[] = [];
  let answered = 0;
  const stub: typeof globalThis.fetch = (input) => {
    urls.push(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url);
    const next = bodies[answered];
    answered += 1;
    if (next instanceof Error) {
      return Promise.reject(next);
    }
    if (next === undefined) {
      return Promise.reject(new Error('the double ran out of canned bodies'));
    }
    return Promise.resolve(new Response(next.slice(), { status: 200 }));
  };
  return { fetch: stub, urls };
}

/** Captures the blobs `URL.createObjectURL` is handed, returning a stable marker URL for each. */
function objectUrlRecorder(): { created: Blob[]; restore: () => void } {
  const created: Blob[] = [];
  const original = URL.createObjectURL.bind(URL);
  Reflect.set(URL, 'createObjectURL', (blob: Blob) => {
    created.push(blob);
    return `blob:stub-${created.length}`;
  });
  return { created, restore: () => Reflect.set(URL, 'createObjectURL', original) };
}

test('a sealed object is fetched, opened, and cached as a blob typed with the claim', async () => {
  const { client, calls } = uploadRecorder();
  const bytes = new Uint8Array([11, 22, 33, 44]);
  const content = await uploadImageAttachment(client, 'conv_1' as Id, imageFile(bytes));
  const sealed = (calls[0] as UploadCall)[1];

  const { fetch, urls } = fetchRecorder([sealed]);
  Reflect.set(globalThis, 'fetch', fetch);
  const { created, restore } = objectUrlRecorder();
  try {
    const first = await resolveMediaObject(client, content);
    assert.equal(first, 'blob:stub-1');
    assert.equal(urls.length, 1, 'one fetch for the one object');
    assert.equal(created.length, 1, 'one blob for the one object');
    assert.equal(created[0]?.type, 'image/png', 'the blob is typed with the sender claim');
    const opened = created[0];
    assert.ok(opened !== undefined, 'the object was handed to createObjectURL');
    assert.deepEqual(
      new Uint8Array(await opened.arrayBuffer()),
      bytes,
      'the object the renderer embeds is the opened plaintext, not the stored ciphertext',
    );

    // The bytes never change, so the second resolve is served from the session cache.
    assert.equal(await resolveMediaObject(client, content), 'blob:stub-1');
    assert.equal(urls.length, 1, 'a cached object must not be refetched');
  } finally {
    restore();
  }
});

test("a legacy zero-key message's bytes pass through unopened", async () => {
  const client = uploadRecorder().client;
  const stored = new Uint8Array([5, 4, 3, 2, 1]);
  const content: Pick<MediaRefContent, 'type' | 'mediaId' | 'mimeType' | 'key' | 'nonce'> = {
    type: ContentType.MediaRef,
    mediaId: 'media_legacy' as Id,
    mimeType: 'image/png',
    key: LEGACY_PLAINTEXT_SLOTS.key,
    nonce: LEGACY_PLAINTEXT_SLOTS.nonce,
  };
  const { fetch } = fetchRecorder([stored]);
  Reflect.set(globalThis, 'fetch', fetch);
  const { created, restore } = objectUrlRecorder();
  try {
    assert.equal(await resolveMediaObject(client, content), 'blob:stub-1');
    const blob = created[0];
    assert.ok(blob !== undefined, 'the legacy object was handed to createObjectURL');
    assert.deepEqual(
      new Uint8Array(await blob.arrayBuffer()),
      stored,
      'a pre-sealing message has no seal to open; its bytes are the object',
    );
  } finally {
    restore();
  }
});

test('a failed object resolution is not cached, so the next render retries', async () => {
  const client = uploadRecorder().client;
  const sealed = sealing.seal(new Uint8Array([1, 2, 3, 4]), new TextEncoder().encode('migo-media'));
  const content: Pick<MediaRefContent, 'type' | 'mediaId' | 'mimeType' | 'key' | 'nonce'> = {
    type: ContentType.MediaRef,
    mediaId: 'media_object_retry' as Id,
    mimeType: 'image/png',
    key: sealed.key,
    nonce: sealed.nonce,
  };
  const { fetch, urls } = fetchRecorder([new Error('data plane unreachable'), sealed.sealed]);
  Reflect.set(globalThis, 'fetch', fetch);
  const { created, restore } = objectUrlRecorder();
  try {
    await assert.rejects(resolveMediaObject(client, content));
    assert.equal(await resolveMediaObject(client, content), 'blob:stub-1');
    assert.equal(urls.length, 2, 'the failed fetch must not poison the object');
    const blob = created[0];
    assert.ok(blob !== undefined, 'the retried object was handed to createObjectURL');
    assert.deepEqual(
      new Uint8Array(await blob.arrayBuffer()),
      new Uint8Array([1, 2, 3, 4]),
      'the retry opens the object it fetched',
    );
  } finally {
    restore();
  }
});
