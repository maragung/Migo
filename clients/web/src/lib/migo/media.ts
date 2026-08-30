'use client';

/**
 * Media attachments in the web client: turn a picked `File` into an uploaded object and a message
 * that references it, and resolve the short-lived download URLs the message list renders.
 *
 * The split mirrors the SDK's: an upload is one convenience call ({@link uploadImageAttachment}
 * hides begin/PUT/commit), while rendering resolves a URL per object through {@link resolveMediaUrl}
 * — a session-wide cache, because a signed URL outlives any one conversation view, and a URL is
 * refetched only when its expiry is near, not every time a message re-renders.
 *
 * # The placeholder key material
 *
 * A `MediaRefContent` carries the symmetric `key` and `nonce` that open the object, because the
 * content contract is end-to-end. Sealing media before upload is a separate, future feature; until
 * it lands the bytes are stored as uploaded, the renderer downloads by `mediaId` without
 * decrypting, and the key slots carry placeholder material so the message shape is already the
 * final one. Swapping the placeholders for real key material later touches nothing else here.
 */

import { ContentType, MediaKind } from '@migo/sdk';
import type { Id, MediaRefContent, UploadResult } from '@migo/sdk';

/**
 * The slice of the client the media helpers need, so a caller (or a test) can supply any object
 * with these two methods rather than a whole {@link MigoClient}.
 */
export interface MediaClient {
  readonly media: {
    upload(
      options: {
        kind: MediaKind;
        contentType: string;
        size: number;
        conversationId?: Id;
        width?: number;
        height?: number;
        durationMs?: number;
      },
      bytes: Uint8Array,
    ): Promise<UploadResult>;
    download(objectId: Id): Promise<{ url: string; expiresAt: number }>;
  };
}

/**
 * Placeholder key material for the media key slots; see the module doc.
 * Zero-filled and of the lengths the future encryption will use, so the message shape is final.
 */
const PLACEHOLDER_MEDIA_KEY = new Uint8Array(32);
const PLACEHOLDER_MEDIA_NONCE = new Uint8Array(12);

/** Reads a `File` fully into bytes, the shape the media data plane PUTs. */
export async function readFileBytes(file: File): Promise<Uint8Array> {
  return new Uint8Array(await file.arrayBuffer());
}

/**
 * The pixel dimensions of an image file, or `null` when they cannot be read.
 *
 * Decoding the image locally is what lets the message carry `width`/`height` — a receiver can lay
 * out the bubble before downloading anything — and a file that will not decode reports `null`
 * rather than failing the whole upload: the server's own byte sniff is the authority anyway.
 */
export async function imageDimensions(
  file: File,
): Promise<{ width: number; height: number } | null> {
  const url = URL.createObjectURL(file);
  try {
    return await new Promise<{ width: number; height: number } | null>((resolve) => {
      const image = new Image();
      image.onload = () => resolve({ width: image.naturalWidth, height: image.naturalHeight });
      image.onerror = () => resolve(null);
      image.src = url;
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** The MIME type to claim for a picked file: its own type, or a neutral one when the browser has none. */
function claimMime(file: File): string {
  return file.type === '' ? 'application/octet-stream' : file.type;
}

/**
 * The message body for an uploaded image: the reference the receiver renders, in the sender's
 * claim of type and dimensions, with the placeholder key material (see the module doc).
 *
 * Extracted from {@link uploadImageAttachment} so the content shape is a pure function a test can
 * pin — the placeholder rule especially, since "these bytes are not really encrypted yet" is a
 * fact a future change must replace deliberately, not drift away from.
 */
export function imageAttachmentContent(
  uploaded: UploadResult,
  claim: { mimeType: string; sizeBytes: number; width?: number; height?: number },
): MediaRefContent {
  const content: MediaRefContent = {
    type: ContentType.MediaRef,
    mediaId: uploaded.mediaId,
    mimeType: claim.mimeType,
    sizeBytes: claim.sizeBytes,
    key: PLACEHOLDER_MEDIA_KEY,
    nonce: PLACEHOLDER_MEDIA_NONCE,
  };
  if (claim.width !== undefined) {
    content.width = claim.width;
  }
  if (claim.height !== undefined) {
    content.height = claim.height;
  }
  return content;
}

/**
 * Uploads a picked image file into a conversation and returns the message body that references it.
 *
 * The file's own dimensions, when they can be read, ride both the upload (so the server's record
 * carries them) and the message (so receivers lay out before downloading).
 */
export async function uploadImageAttachment(
  client: MediaClient,
  conversationId: Id,
  file: File,
): Promise<MediaRefContent> {
  const [bytes, dimensions] = await Promise.all([readFileBytes(file), imageDimensions(file)]);
  const uploaded = await client.media.upload(
    {
      kind: MediaKind.Image,
      contentType: claimMime(file),
      size: bytes.length,
      conversationId,
      ...(dimensions ?? {}),
    },
    bytes,
  );
  return imageAttachmentContent(uploaded, {
    mimeType: claimMime(file),
    sizeBytes: bytes.length,
    ...(dimensions ?? {}),
  });
}

/**
 * Uploads a picked image file as the caller's new avatar and returns its media id, for
 * `profile.updateProfile({ avatarMediaId })`.
 *
 * Avatar uploads are profile-scoped — no conversation id — because an avatar's audience is whoever
 * may see the profile, not a conversation's members.
 */
export async function uploadAvatarMedia(client: MediaClient, file: File): Promise<Id> {
  const bytes = await readFileBytes(file);
  const uploaded = await client.media.upload(
    { kind: MediaKind.Avatar, contentType: claimMime(file), size: bytes.length },
    bytes,
  );
  return uploaded.mediaId;
}

/** One resolved download URL and the moment it stops working. */
interface CachedMediaUrl {
  url: string;
  expiresAt: number;
}

/** Session-wide: a media id to its resolved URL, so re-renders never refetch. */
const mediaUrlCache = new Map<Id, CachedMediaUrl>();
/** A media id to the download already in flight, so concurrent bubbles share one request. */
const mediaUrlInFlight = new Map<Id, Promise<string>>();

/** Refresh a URL this close to its expiry, so a render never races the deadline. */
const URL_EXPIRY_SKEW_MS = 30_000;

/**
 * Resolves a media object to a URL the caller can fetch or embed, for this session.
 *
 * Cached per media id until the URL is near expiry; concurrent requests for the same id share one
 * download. A failure is not cached — the next call tries again, because a media server briefly
 * unavailable is not a verdict about the object.
 */
export async function resolveMediaUrl(client: MediaClient, mediaId: Id): Promise<string> {
  const cached = mediaUrlCache.get(mediaId);
  if (cached !== undefined && cached.expiresAt > Date.now() + URL_EXPIRY_SKEW_MS) {
    return cached.url;
  }
  const inFlight = mediaUrlInFlight.get(mediaId);
  if (inFlight !== undefined) {
    return inFlight;
  }
  const pending = client.media
    .download(mediaId)
    .then((granted) => {
      mediaUrlCache.set(mediaId, { url: granted.url, expiresAt: granted.expiresAt });
      return granted.url;
    })
    .finally(() => {
      mediaUrlInFlight.delete(mediaId);
    });
  mediaUrlInFlight.set(mediaId, pending);
  return pending;
}
