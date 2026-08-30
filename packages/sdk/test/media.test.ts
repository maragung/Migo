/**
 * The media domain: the control-plane opcodes over the recording transport, the data-plane PUT
 * over an injected fetch double.
 *
 * Like {@link domains.test.ts}, every control-plane test drives the real {@link Rpc} against the
 * {@link RecordingTransport} double, so both halves of each method are exercised against the
 * generated codecs: what the domain *sent* is decoded back out of the recorded frame body, and what
 * it *returned* is decoded from a reply the test encoded.
 *
 * The data plane gets its own double. {@link MediaDomain.uploadBytes} rides plain HTTP, so the test
 * injects a `fetch` that records the URL, method, headers, and body it was handed and answers a real
 * `Response`. The one convenience method, {@link MediaDomain.upload}, is then tested end-to-end
 * across both planes: the PUT's URL must be the one the ticket carried, the commit's digest must be
 * the SHA-256 of the exact bytes sent (checked against a known vector rather than a re-computation,
 * so a hash regression cannot hide), and a PUT that fails must abort the ticket rather than leave a
 * half-written object on the server.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { decodeBody, encodeBody, MediaDomain, MediaKind, RemoteError, Rpc } from '../src/index.js';
import type { FetchLike, UploadOptions } from '../src/index.js';
import { OP } from '@migo/protocol';
import {
  decodeMediaAbort,
  decodeMediaBegin,
  decodeMediaCommit,
  decodeMediaFetch,
  decodeMediaStatusReq,
  encodeAcknowledged,
  encodeMediaProgress,
  encodeMediaTicket,
  encodeMediaUrl,
} from '@migo/protocol';
import type { Id } from '@migo/wire';

import { RecordingTransport, idOf } from './harness.js';

/** One HTTP request the data-plane double observed. */
interface RecordedPut {
  url: string;
  method: string;
  contentType: string | undefined;
  body: Uint8Array | undefined;
}

/** A `fetch` that records what it was handed and answers 200 with no body. */
function recordingFetch(log: RecordedPut[]): FetchLike {
  return (url, init) => {
    const headers = new Headers(init?.headers);
    log.push({
      url,
      method: init?.method ?? 'GET',
      contentType: headers.get('content-type') ?? undefined,
      body: init?.body instanceof Uint8Array ? init.body : undefined,
    });
    return Promise.resolve(new Response(null, { status: 200 }));
  };
}

/** A `fetch` that records and then fails with the given status and JSON body. */
function failingFetch(log: RecordedPut[], status: number, body: unknown): FetchLike {
  return (url, init) => {
    const headers = new Headers(init?.headers);
    log.push({
      url,
      method: init?.method ?? 'GET',
      contentType: headers.get('content-type') ?? undefined,
      body: init?.body instanceof Uint8Array ? init.body : undefined,
    });
    return Promise.resolve(new Response(JSON.stringify(body), { status }));
  };
}

/** Builds a media domain over one recording transport, with per-opcode canned replies. */
function rig(
  replies: Map<number, (body: Uint8Array) => Uint8Array>,
  fetch: FetchLike = recordingFetch([]),
): { transport: RecordingTransport; media: MediaDomain } {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, media: new MediaDomain(rpc, fetch) };
}

/** A reply that hands back a fixed upload ticket. */
function ticketReply(ticket: { uploadId: Id; uploadUrl: string; headers?: string[] }) {
  return () =>
    encodeBody(encodeMediaTicket, {
      uploadId: ticket.uploadId,
      uploadUrl: ticket.uploadUrl,
      headers: ticket.headers ?? [],
    });
}

function sentAt(
  transport: RecordingTransport,
  index: number,
): { opcode: number; body: Uint8Array } {
  const frame = transport.sent[index];
  assert.ok(frame !== undefined, `expected a recorded frame at index ${index}`);
  return frame;
}

const UPLOAD_ID = idOf(31);
const CONVERSATION = idOf(7);
const UPLOAD_URL = 'https://media.example.test/media/avatar/0000ff?key=signed';
/** A Unix-ms instant after the Migo epoch (2024-01-01), so timestamps round-trip through the codec. */
const AT = 1_767_225_600_000;

/** The full options set, so the "carries every optional" test spells each field out once. */
const FULL_OPTIONS: UploadOptions = {
  kind: MediaKind.Image,
  contentType: 'image/png',
  size: 4096,
  conversationId: CONVERSATION,
  width: 1920,
  height: 1080,
  durationMs: 60_000,
};

test('begin sends MEDIA_UPLOAD_BEGIN with kind, type, size, and every optional', async () => {
  const { transport, media } = rig(
    new Map([[OP.MEDIA_UPLOAD_BEGIN, ticketReply({ uploadId: UPLOAD_ID, uploadUrl: UPLOAD_URL })]]),
  );
  const ticket = await media.begin(FULL_OPTIONS);
  assert.deepEqual(ticket, { uploadId: UPLOAD_ID, uploadUrl: UPLOAD_URL, headers: [] });
  assert.equal(sentAt(transport, 0).opcode, OP.MEDIA_UPLOAD_BEGIN);
  assert.deepEqual(decodeBody(decodeMediaBegin, sentAt(transport, 0).body), {
    kind: MediaKind.Image,
    contentType: 'image/png',
    size: 4096,
    conversationId: CONVERSATION,
    width: 1920,
    height: 1080,
    durationMs: 60_000,
  });
});

test('begin omits the optionals the caller did not claim, so profile media stays profile-scoped', async () => {
  const { transport, media } = rig(
    new Map([[OP.MEDIA_UPLOAD_BEGIN, ticketReply({ uploadId: UPLOAD_ID, uploadUrl: UPLOAD_URL })]]),
  );
  await media.begin({ kind: MediaKind.Avatar, contentType: 'image/png', size: 128 });
  // A decode of a struct whose optionals were never written yields exactly the required fields;
  // an avatar must not smuggle a conversation id onto the wire.
  assert.deepEqual(decodeBody(decodeMediaBegin, sentAt(transport, 0).body), {
    kind: MediaKind.Avatar,
    contentType: 'image/png',
    size: 128,
  });
});

test('uploadBytes PUTs the exact bytes as an opaque octet stream', async () => {
  const log: RecordedPut[] = [];
  const { media } = rig(new Map(), recordingFetch(log));
  const bytes = new TextEncoder().encode('hello');
  await media.uploadBytes(UPLOAD_URL, bytes);
  assert.equal(log.length, 1);
  assert.equal(log[0]?.url, UPLOAD_URL);
  assert.equal(log[0]?.method, 'PUT');
  // The data plane never claims to know what the bytes are; the real type claim rode the ticket.
  assert.equal(log[0]?.contentType, 'application/octet-stream');
  assert.equal(log[0]?.body?.length, bytes.length);
  assert.ok(log[0]?.body !== undefined && bytes.every((byte, i) => log[0]!.body![i] === byte));
});

test('uploadBytes turns a non-2xx answer into a RemoteError from the envelope', async () => {
  const { media } = rig(
    new Map(),
    failingFetch([], 403, { error: { code: 1603, symbol: 'MEDIA_UNAVAILABLE', message: '' } }),
  );
  await assert.rejects(media.uploadBytes(UPLOAD_URL, new Uint8Array(4)), (cause: unknown) => {
    assert.ok(cause instanceof RemoteError);
    assert.equal(cause.code, 1603);
    assert.equal(cause.symbol, 'MEDIA_UNAVAILABLE');
    return true;
  });
});

test('status sends the upload id and decodes the byte counters', async () => {
  const { transport, media } = rig(
    new Map([
      [
        OP.MEDIA_UPLOAD_STATUS,
        () => encodeBody(encodeMediaProgress, { received: 512, expected: 4096 }),
      ],
    ]),
  );
  const progress = await media.status(UPLOAD_ID);
  assert.deepEqual(progress, { received: 512, expected: 4096 });
  assert.deepEqual(decodeBody(decodeMediaStatusReq, sentAt(transport, 0).body), {
    uploadId: UPLOAD_ID,
  });
});

test('commit sends the digest and resolves with nothing on the acknowledgement', async () => {
  const digest = new Uint8Array(32).fill(0xab);
  const { transport, media } = rig(
    new Map([[OP.MEDIA_UPLOAD_COMMIT, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  const committed: void = await media.commit(UPLOAD_ID, digest);
  assert.equal(committed, undefined);
  assert.deepEqual(decodeBody(decodeMediaCommit, sentAt(transport, 0).body), {
    uploadId: UPLOAD_ID,
    digest,
  });
});

test('abort sends the upload id and resolves on the acknowledgement', async () => {
  const { transport, media } = rig(
    new Map([[OP.MEDIA_UPLOAD_ABORT, () => encodeBody(encodeAcknowledged, { ok: true })]]),
  );
  await media.abort(UPLOAD_ID);
  assert.deepEqual(decodeBody(decodeMediaAbort, sentAt(transport, 0).body), {
    uploadId: UPLOAD_ID,
  });
});

test('fetchUrl sends the object id, with the conversation only when scoped', async () => {
  const reply = () => encodeBody(encodeMediaUrl, { url: UPLOAD_URL, expiresAt: AT });
  const { transport, media } = rig(new Map([[OP.MEDIA_FETCH_URL, reply]]));
  assert.deepEqual(await media.fetchUrl(UPLOAD_ID), { url: UPLOAD_URL, expiresAt: AT });
  assert.deepEqual(decodeBody(decodeMediaFetch, sentAt(transport, 0).body), {
    objectId: UPLOAD_ID,
  });

  assert.deepEqual(await media.fetchUrl(UPLOAD_ID, CONVERSATION), {
    url: UPLOAD_URL,
    expiresAt: AT,
  });
  assert.deepEqual(decodeBody(decodeMediaFetch, sentAt(transport, 1).body), {
    objectId: UPLOAD_ID,
    conversationId: CONVERSATION,
  });
});

test('download flattens the granted URL to what a caller acts on', async () => {
  const { media } = rig(
    new Map([
      [OP.MEDIA_FETCH_URL, () => encodeBody(encodeMediaUrl, { url: UPLOAD_URL, expiresAt: AT })],
    ]),
  );
  assert.deepEqual(await media.download(UPLOAD_ID), { url: UPLOAD_URL, expiresAt: AT });
});

test('upload runs begin, PUT, and commit, and commits the SHA-256 of the bytes', async () => {
  const log: RecordedPut[] = [];
  const bytes = new TextEncoder().encode('hello');
  const { transport, media } = rig(
    new Map([
      [OP.MEDIA_UPLOAD_BEGIN, ticketReply({ uploadId: UPLOAD_ID, uploadUrl: UPLOAD_URL })],
      [OP.MEDIA_UPLOAD_COMMIT, () => encodeBody(encodeAcknowledged, { ok: true })],
    ]),
    recordingFetch(log),
  );
  const result = await media.upload(FULL_OPTIONS, bytes);
  assert.deepEqual(result, { mediaId: UPLOAD_ID });

  // The data plane PUT went to the URL the ticket carried, carrying exactly the caller's bytes.
  assert.equal(log.length, 1);
  assert.equal(log[0]?.url, UPLOAD_URL);
  assert.equal(log[0]?.body?.length, bytes.length);

  // The commit's digest is the known SHA-256 of b"hello" — a vector, not a re-computation, so a
  // hash regression cannot make this test agree with the bug.
  const commit = decodeBody(decodeMediaCommit, sentAt(transport, 1).body);
  assert.equal(commit.uploadId, UPLOAD_ID);
  assert.equal(
    Buffer.from(commit.digest).toString('hex'),
    '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824',
  );
});

test('upload aborts the ticket when the PUT fails, and rethrows the original error', async () => {
  const { transport, media } = rig(
    new Map([
      [OP.MEDIA_UPLOAD_BEGIN, ticketReply({ uploadId: UPLOAD_ID, uploadUrl: UPLOAD_URL })],
      [OP.MEDIA_UPLOAD_ABORT, () => encodeBody(encodeAcknowledged, { ok: true })],
    ]),
    failingFetch([], 500, { error: { code: 1000, symbol: 'INTERNAL_ERROR', message: 'boom' } }),
  );
  await assert.rejects(
    media.upload({ kind: MediaKind.Image, contentType: 'image/png', size: 5 }, new Uint8Array(5)),
    (cause: unknown) => cause instanceof RemoteError && cause.symbol === 'INTERNAL_ERROR',
  );
  // begin, then the abort that releases the half-written object; no commit was ever sent.
  assert.equal(transport.sent.length, 2);
  assert.equal(sentAt(transport, 1).opcode, OP.MEDIA_UPLOAD_ABORT);
  assert.deepEqual(decodeBody(decodeMediaAbort, sentAt(transport, 1).body), {
    uploadId: UPLOAD_ID,
  });
});

test('upload that fails at begin sends nothing further', async () => {
  const log: RecordedPut[] = [];
  const { transport, media } = rig(new Map(), recordingFetch(log));
  // No canned reply for MEDIA_UPLOAD_BEGIN: the double answers an empty body, which fails to
  // decode as a ticket, standing in for a refused begin.
  await assert.rejects(
    media.upload({ kind: MediaKind.Image, contentType: 'image/png', size: 1 }, new Uint8Array(1)),
  );
  assert.equal(transport.sent.length, 1);
  assert.equal(log.length, 0);
});
