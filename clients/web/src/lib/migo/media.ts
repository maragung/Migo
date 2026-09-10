'use client';

/**
 * Media attachments in the web client: turn a picked `File` into a sealed upload and a message
 * that references it, and resolve the short-lived download URLs the message list renders through.
 *
 * The split mirrors the SDK's: an upload is one convenience call ({@link uploadImageAttachment}
 * hides seal/begin/PUT/commit), while rendering resolves an object per message through {@link
 * resolveMediaObject} — a session-wide cache, because a decrypted object outlives any one
 * conversation view, and the bytes are fetched and opened once, not every time a message
 * re-renders.
 *
 * # The sealing, and where its key lives
 *
 * Media is ciphertext at rest, as the brief mandates: {@link uploadImageAttachment} mints a fresh
 * symmetric key per upload, seals the bytes with the house AEAD (XChaCha20-Poly1305 — see
 * `@migo/crypto`'s `sealing` module for why not AES-GCM), and uploads only the sealed bytes. The
 * key and nonce ride in the `MediaRefContent`'s own slots, *inside the message* — and the message
 * itself is sealed end-to-end by the messaging layer (`GroupCrypto.sealContent`), so the key
 * reaches exactly the devices that can read the conversation and nobody else. The server stores
 * and serves an object it cannot open, which is the shape section 69 asks for: for E2E media the
 * server validates size, quota, rate, and authorization, and the content is none of its business.
 *
 * A sealed upload declares itself honestly: the stored object's claimed type is
 * `application/octet-stream`, because that is what the bytes are. The *message* still carries the
 * sender's `mimeType` claim — it is what the receiver labels the decrypted blob with, and section
 * 122's rule holds in the only form that can: after decryption, since the server never sees the
 * plaintext to validate it.
 *
 * # The two places that stay plaintext
 *
 * **Legacy messages.** An older build stored bytes as uploaded and put zero-filled placeholder
 * material in the key slots. A message whose key slot is all zeros is one of those: the renderer
 * ({@link resolveMediaObject}) detects it and hands the bytes through unopened, so every old
 * message keeps rendering. The detection is sound because a sealed upload's key is 256 random
 * bits — an all-zero key is not a key anyone sealed under.
 *
 * **Server-readable destinations.** A public or managed room's conversation is `Transport` — the
 * server may read it, and its media policy (content identification, scanning) is deliberate and
 * server-side. Sealing into such a room would be refused at commit, correctly; so the upload
 * helpers take the conversation's end-to-end-ness from the caller and keep the legacy plaintext
 * path for the rooms. Every direct conversation and group is end-to-end by construction — there
 * is no request field that could ask for one that is not — so in practice the plaintext path is
 * the room exception, not the rule. Avatars ({@link uploadAvatarMedia}) are plaintext for a
 * different reason: an avatar's audience is every account that can see the profile, and there is
 * no end-to-end channel that could carry an avatar's key to them.
 */

import { ContentType, MediaKind, sealing } from '@migo/sdk';
import type { Id, MediaRefContent, UploadResult, VoiceNoteRefContent } from '@migo/sdk';

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

/** The associated-data domain of an image attachment: a media blob never opens as a voice note. */
const MEDIA_DOMAIN = new TextEncoder().encode('migo-media');
/**
 * The associated-data domain of a voice note, shared with `voice.ts` — the uploader there seals
 * under it, and {@link resolveMediaObject} here opens under it, and the two must be one constant
 * or a voice note sealed in one module would refuse to open in the other.
 */
export const VOICE_SEAL_DOMAIN = new TextEncoder().encode('migo-voice');

/** What an upload helper needs to know about where the bytes are going. */
export interface UploadDestination {
  /**
   * Whether the conversation is end-to-end, which decides whether the bytes are sealed. Callers
   * derive it from the conversation summary's encryption mode; when unknown, the honest default
   * is `true` — every direct conversation and group is end-to-end, and a room always carries its
   * mode in the summary.
   */
  endToEnd?: boolean;
}

/**
 * The zero key slots a legacy (pre-sealing) message carries, so the plaintext path can build the
 * same content shape the old build sent — and so a test can pin that the legacy path is the only
 * writer of zero material.
 */
export const LEGACY_PLAINTEXT_SLOTS: Readonly<{ key: Uint8Array; nonce: Uint8Array }> = {
  key: new Uint8Array(32),
  nonce: new Uint8Array(24),
};

/**
 * Whether a message's key slot is the zero-filled placeholder material of a legacy upload: bytes
 * stored as uploaded, no sealing, the renderer must not attempt to open them. See the module doc
 * for why an all-zero key can only ever mean this.
 */
export function isLegacyPlaintext(key: Uint8Array): boolean {
  let allZero = true;
  for (const byte of key) {
    if (byte !== 0) {
      allZero = false;
      break;
    }
  }
  return allZero;
}

/** Reads a `File` fully into bytes, the shape the media data plane PUTs. */
export async function readFileBytes(file: File): Promise<Uint8Array> {
  return new Uint8Array(await file.arrayBuffer());
}

/**
 * The pixel dimensions of an image file, or `null` when they cannot be read.
 *
 * Decoding the image locally is what lets the message carry `width`/`height` — a receiver can lay
 * out the bubble before downloading anything — and a file that will not decode reports `null`
 * rather than failing the whole upload. This runs on the plaintext `File`, before sealing, which
 * is the only place the dimensions could come from: the sealed bytes are opaque to everybody,
 * including this client's own later code.
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
 * claim of type and dimensions, with the key material that opens the sealed object.
 *
 * Extracted from {@link uploadImageAttachment} so the content shape is a pure function a test can
 * pin — the key and nonce the upload sealed under must land in the slots verbatim, because they
 * are the only copies that ever reach the receiver. `keyMaterial` is the upload's own output; the
 * legacy plaintext path passes {@link LEGACY_PLAINTEXT_SLOTS} instead, and a receiver tells the
 * two apart with {@link isLegacyPlaintext}.
 */
export function imageAttachmentContent(
  uploaded: UploadResult,
  claim: { mimeType: string; sizeBytes: number; width?: number; height?: number },
  keyMaterial: { key: Uint8Array; nonce: Uint8Array },
): MediaRefContent {
  const content: MediaRefContent = {
    type: ContentType.MediaRef,
    mediaId: uploaded.mediaId,
    mimeType: claim.mimeType,
    sizeBytes: claim.sizeBytes,
    key: keyMaterial.key,
    nonce: keyMaterial.nonce,
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
 * In an end-to-end conversation the bytes are sealed before anything crosses the wire — the
 * server holds ciphertext, and the key rides to the conversation's devices inside the sealed
 * message the caller sends next. In a server-readable room the legacy plaintext path applies (see
 * the module doc). The file's own dimensions, when they can be read, ride both the upload and the
 * message, so receivers can lay out before downloading.
 */
export async function uploadImageAttachment(
  client: MediaClient,
  conversationId: Id,
  file: File,
  destination: UploadDestination = {},
): Promise<MediaRefContent> {
  const [bytes, dimensions] = await Promise.all([readFileBytes(file), imageDimensions(file)]);
  const mime = claimMime(file);

  if (destination.endToEnd === false) {
    const uploaded = await client.media.upload(
      {
        kind: MediaKind.Image,
        contentType: mime,
        size: bytes.length,
        conversationId,
        ...(dimensions ?? {}),
      },
      bytes,
    );
    return imageAttachmentContent(
      uploaded,
      { mimeType: mime, sizeBytes: bytes.length, ...(dimensions ?? {}) },
      LEGACY_PLAINTEXT_SLOTS,
    );
  }

  const sealed = sealing.seal(bytes, MEDIA_DOMAIN);
  const uploaded = await client.media.upload(
    {
      kind: MediaKind.Image,
      // The stored object is opaque ciphertext; the honest claim for it is the neutral one.
      contentType: 'application/octet-stream',
      size: sealed.sealed.length,
      conversationId,
      ...(dimensions ?? {}),
    },
    sealed.sealed,
  );
  return imageAttachmentContent(
    uploaded,
    // The message's claim describes the *content*, not the container: sizeBytes is the plaintext
    // size and mimeType is what the decrypted blob will be labelled with.
    { mimeType: mime, sizeBytes: bytes.length, ...(dimensions ?? {}) },
    sealed,
  );
}

/**
 * The ceiling a document attachment may not exceed, matching the server's `Document` media kind.
 * Checked before any upload call so an oversized file fails with an honest sentence instead of a
 * server round-trip that was always going to refuse.
 */
export const DOCUMENT_MAX_BYTES = 32 * 1024 * 1024;

/**
 * The message body for an uploaded document: the same `MediaRef` shape an image rides, in the
 * sender's claim of type and size, with the key material that opens the sealed object.
 *
 * Extracted from {@link uploadDocumentAttachment} for the same reason {@link imageAttachmentContent}
 * is: the content shape is a pure function a test can pin.
 */
export function documentAttachmentContent(
  uploaded: UploadResult,
  claim: { mimeType: string; sizeBytes: number; fileName: string },
  keyMaterial: { key: Uint8Array; nonce: Uint8Array },
): MediaRefContent {
  return {
    type: ContentType.MediaRef,
    mediaId: uploaded.mediaId,
    mimeType: claim.mimeType,
    sizeBytes: claim.sizeBytes,
    key: keyMaterial.key,
    nonce: keyMaterial.nonce,
    caption: claim.fileName,
  };
}

/**
 * Uploads a picked document file into an end-to-end conversation and returns the message body that
 * references it.
 *
 * Documents are a private-and-group feature, and every direct conversation and group is
 * end-to-end, so there is no plaintext branch to keep: the bytes are always sealed (see the module
 * doc), and a caller that reaches here from a room is a caller that ignored the composer's gating.
 * The file's name rides in the `MediaRef`'s caption slot — the only free-text field the content
 * shape has — and the receiver's renderer tells a document from an image by the claimed MIME type.
 *
 * @throws RangeError when the file exceeds {@link DOCUMENT_MAX_BYTES}, before anything is uploaded.
 */
export async function uploadDocumentAttachment(
  client: MediaClient,
  conversationId: Id,
  file: File,
): Promise<MediaRefContent> {
  const bytes = await readFileBytes(file);
  if (bytes.length > DOCUMENT_MAX_BYTES) {
    throw new RangeError(
      `migo: document is ${bytes.length} bytes, over the ${DOCUMENT_MAX_BYTES} byte ceiling`,
    );
  }
  const mime = claimMime(file);
  const sealed = sealing.seal(bytes, MEDIA_DOMAIN);
  const uploaded = await client.media.upload(
    {
      kind: MediaKind.Document,
      // The stored object is opaque ciphertext; the honest claim for it is the neutral one.
      contentType: 'application/octet-stream',
      size: sealed.sealed.length,
      conversationId,
    },
    sealed.sealed,
  );
  return documentAttachmentContent(
    uploaded,
    { mimeType: mime, sizeBytes: bytes.length, fileName: file.name },
    sealed,
  );
}

/**
 * Uploads a picked image file as the caller's new avatar and returns its media id, for
 * `profile.updateProfile({ avatarMediaId })`.
 *
 * Avatar uploads are profile-scoped — no conversation id — because an avatar's audience is whoever
 * may see the profile, not a conversation's members. That audience is also why the bytes stay
 * plaintext: an avatar is server-readable by design (any authenticated account may render one),
 * and there is no end-to-end channel that could carry an avatar's key to that audience.
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

/**
 * One decrypted object, held as the object URL a renderer embeds.
 *
 * A blob URL has no expiry — the bytes are already in hand — so `expiresAt` is absent and the
 * cache entry lives for the session, unlike {@link CachedMediaUrl}.
 */
interface CachedMediaObject {
  url: string;
}

/** Session-wide: a media id to its decrypted object URL. The bytes never change, so neither does the entry. */
const mediaObjectCache = new Map<Id, CachedMediaObject>();
/** A media id to the fetch-and-open already in flight, so concurrent bubbles share one download. */
const mediaObjectInFlight = new Map<Id, Promise<string>>();

/** The fetch the data-plane download uses; resolved lazily so tests can substitute one first. */
function byteFetch(): typeof globalThis.fetch {
  const impl = globalThis.fetch;
  if (impl === undefined) {
    throw new TypeError('migo: no fetch implementation available to download media bytes');
  }
  return impl;
}

/**
 * Fetches one media object, opens it, and returns an object URL the renderer can embed.
 *
 * This is the renderer's whole contract with the encryption: resolve a signed URL ({@link
 * resolveMediaUrl}), fetch the bytes, and either open them with the message's key and nonce
 * (sealed upload) or pass them through (legacy plaintext, detected with {@link isLegacyPlaintext}).
 * What comes back is a blob URL typed with the sender's claimed `mimeType` — the claim is
 * authenticated by the message's own end-to-end seal, and it is a label for playback, never a
 * fact the client acts on beyond handing it to the decoder.
 *
 * The domain label follows the message's content type, because that is what decided it at seal
 * time: a `VoiceNoteRef` opens under the voice domain, everything else under the media domain.
 * Cached and shared in flight per media id exactly like {@link resolveMediaUrl}; a failure is not
 * cached, because a failed download or a failed open says nothing durable about the object.
 *
 * @throws when the download, fetch, or decryption fails — the caller decides how a broken
 *   object degrades (the chat window resolves it to `null` and the bubble keeps its placeholder).
 */
export async function resolveMediaObject(
  client: MediaClient,
  content: Pick<
    MediaRefContent | VoiceNoteRefContent,
    'type' | 'mediaId' | 'mimeType' | 'key' | 'nonce'
  >,
): Promise<string> {
  const cached = mediaObjectCache.get(content.mediaId);
  if (cached !== undefined) {
    return cached.url;
  }
  const inFlight = mediaObjectInFlight.get(content.mediaId);
  if (inFlight !== undefined) {
    return inFlight;
  }
  const pending = (async (): Promise<string> => {
    const url = await resolveMediaUrl(client, content.mediaId);
    const response = await byteFetch()(url);
    if (!response.ok) {
      throw new Error(`migo: media download answered ${response.status}`);
    }
    const stored = new Uint8Array(await response.arrayBuffer());
    const domain = content.type === ContentType.VoiceNoteRef ? VOICE_SEAL_DOMAIN : MEDIA_DOMAIN;
    const opened = isLegacyPlaintext(content.key)
      ? stored
      : sealing.open(content.key, content.nonce, domain, stored);
    // The copy is for the type system as much as the bytes: a Blob part must own an ArrayBuffer,
    // and an opened AEAD output is typed over the looser buffer family.
    const owned = new Uint8Array(opened);
    const objectUrl = URL.createObjectURL(new Blob([owned], { type: content.mimeType }));
    mediaObjectCache.set(content.mediaId, { url: objectUrl });
    return objectUrl;
  })().finally(() => {
    mediaObjectInFlight.delete(content.mediaId);
  });
  mediaObjectInFlight.set(content.mediaId, pending);
  return pending;
}
