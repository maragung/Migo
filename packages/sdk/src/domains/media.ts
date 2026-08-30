/**
 * The media domain: the data plane behind message attachments and avatars.
 *
 * Everything else in this SDK rides the gateway socket; media does not. An attachment is too large
 * for a frame, so the protocol splits it in two: a *control plane* of small opcodes over the socket
 * — {@link begin} mints an upload ticket, {@link status} reports how many bytes landed, {@link
 * commit} finalises the object, {@link abort} abandons it, {@link fetchUrl} grants a download URL —
 * and a *data plane* of plain HTTP: one `PUT` of the raw bytes to the signed URL the ticket carried,
 * one `GET` of the signed URL {@link download} returns. The server stores and serves the bytes but
 * never needs to understand them.
 *
 * # The two convenience methods
 *
 * A caller that only wants "these bytes, uploaded" uses {@link upload}: it begins, PUTs, and commits
 * with the SHA-256 of the bytes, and — so a half-written object does not linger on the server —
 * aborts the ticket if any step after `begin` fails. A caller that only wants "show this object"
 * uses {@link download}, which resolves a short-lived URL and hands it back for the caller to fetch
 * itself; the SDK does not stream the body, because who owns the bytes once they arrive (an `<img>`,
 * a disk download, a cache) is the caller's business, not the protocol's.
 *
 * # Encryption is a message-layer concern
 *
 * The bytes cross as `application/octet-stream` and, for now, are exactly what the caller passed.
 * Sealing them with the symmetric key a `MediaRefContent` carries is a future layer; the
 * message-content contract already reserves the `key` and `nonce` slots for it, so adding the
 * sealing later changes neither the wire nor this domain's surface.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeMediaBegin,
  decodeMediaTicket,
  encodeMediaStatusReq,
  decodeMediaProgress,
  encodeMediaCommit,
  encodeMediaAbort,
  encodeMediaFetch,
  decodeMediaUrl,
  decodeAcknowledged,
} from '@migo/protocol';
import type { MediaBegin, MediaFetch, MediaTicket, MediaProgress, MediaUrl } from '@migo/protocol';

import { RemoteError, SdkError } from '../errors.js';
import type { FetchLike } from '../rest.js';
import type { Rpc } from './rpc.js';

/**
 * What a media object *is*, as the server's media policy numbers it.
 *
 * The wire field is a plain `u32` and this enum is the vocabulary both ends agree on. `Avatar` is
 * the zero value: a profile picture is readable by anyone who can see the profile, while every
 * other kind is scoped to the conversation it was uploaded into, so the distinction is an
 * authorisation boundary, not a display hint.
 */
export enum MediaKind {
  /** A profile picture; readable by anyone who can see the profile. */
  Avatar = 0,
  /** A still image. */
  Image = 1,
  /** A video. */
  Video = 2,
  /** Music or a recording that is not a voice note. */
  Audio = 3,
  /** A push-to-talk recording. */
  VoiceNote = 4,
  /** Anything else a user attaches. */
  Document = 5,
}

/** What the caller knows about the bytes it is about to upload. */
export interface UploadOptions {
  /** The content kind, which selects the server's size and scan policy for the object. */
  kind: MediaKind;
  /** The MIME type the caller believes the bytes are; the server checks it against them at commit. */
  contentType: string;
  /** The exact byte count the caller will PUT; the ticket is refused a different number. */
  size: number;
  /** The conversation the object belongs to; omit it for profile media such as an avatar. */
  conversationId?: Id;
  /** The pixel width, when the caller knows it, so recipients can lay out before downloading. */
  width?: number;
  /** The pixel height, when the caller knows it. */
  height?: number;
  /** For audio and video, the playing time in milliseconds. */
  durationMs?: number;
}

/** What a completed upload leaves the caller holding. */
export interface UploadResult {
  /** The object's id — the `upload_id` the ticket carried, now the id to reference and fetch by. */
  mediaId: Id;
  /**
   * A URL the object can already be fetched from, when the server hands one back with the upload
   * itself. The current protocol does not — a URL must be requested through {@link download} — so
   * callers treat this as present-only-when-set and fall back to {@link download}.
   */
  downloadUrl?: string;
}

/**
 * Upload and download media objects.
 *
 * One instance per client. Stateless beyond the injected `fetch`: the caller drives the protocol's
 * resumability itself ({@link status} exists precisely so a caller can decide whether to re-PUT),
 * because where a half-finished upload's bytes are buffered is a caller concern.
 */
export class MediaDomain {
  readonly #rpc: Rpc;
  readonly #fetch: FetchLike;

  /**
   * @param rpc The socket request bridge the control-plane opcodes ride.
   * @param fetch The `fetch` the data-plane PUT rides; defaults to the global one, for a non-browser host.
   */
  constructor(rpc: Rpc, fetch?: FetchLike) {
    this.#rpc = rpc;
    const impl = fetch ?? globalThis.fetch;
    if (impl === undefined) {
      throw new TypeError('MediaDomain needs a fetch implementation: none was found on globalThis');
    }
    // Bind so a global `fetch` is not called with the wrong receiver.
    this.#fetch = fetch ?? impl.bind(globalThis);
  }

  /**
   * Opens an upload ticket: the id that claims the object, the signed URL to PUT to, and any
   * headers the server requires alongside the bytes.
   *
   * The declared `size` is checked against the policy for {@link UploadOptions.kind} here, so an
   * oversized object is refused before a byte crosses the wire. The ticket is short-lived; a PUT
   * after it expires is rejected and the upload must begin again.
   */
  async begin(options: UploadOptions): Promise<MediaTicket> {
    // Copy the set fields into a fresh struct so the wire carries exactly what was claimed, and an
    // explicitly-undefined optional stays absent rather than encoded as a zero.
    const request: MediaBegin = {
      kind: options.kind,
      contentType: options.contentType,
      size: options.size,
    };
    if (options.conversationId !== undefined) {
      request.conversationId = options.conversationId;
    }
    if (options.width !== undefined) {
      request.width = options.width;
    }
    if (options.height !== undefined) {
      request.height = options.height;
    }
    if (options.durationMs !== undefined) {
      request.durationMs = options.durationMs;
    }
    return this.#rpc.call(OP.MEDIA_UPLOAD_BEGIN, encodeMediaBegin, decodeMediaTicket, request);
  }

  /**
   * PUTs the raw bytes to a ticket's upload URL.
   *
   * One request, the whole object: the signed URL names the destination and the content type is
   * always `application/octet-stream`, because the object is an opaque blob to the HTTP layer — its
   * real type is the claim made at {@link begin}, which the server verifies against the bytes at
   * commit. A non-2xx answer becomes a {@link RemoteError}.
   */
  async uploadBytes(url: string, bytes: Uint8Array): Promise<void> {
    // The DOM's `BodyInit` only admits an ArrayBuffer-backed view, while this SDK's convention is
    // the broader `Uint8Array`; every caller in practice holds an ArrayBuffer-backed one, so the
    // assertion narrows the type without touching the bytes.
    const body = bytes as Uint8Array<ArrayBuffer>;
    const response = await this.#fetch(url, {
      method: 'PUT',
      headers: { 'content-type': 'application/octet-stream' },
      body,
    });
    if (!response.ok) {
      let parsed: unknown = null;
      try {
        parsed = await response.json();
      } catch {
        parsed = null;
      }
      throw RemoteError.fromEnvelope(response.status, parsed);
    }
  }

  /**
   * Asks how many of the declared bytes the server has received.
   *
   * For a single-shot PUT this is always "none or all"; it exists for a caller resuming after a
   * dropped connection, which re-PUTs from `received` rather than from zero.
   */
  async status(uploadId: Id): Promise<MediaProgress> {
    return this.#rpc.call(OP.MEDIA_UPLOAD_STATUS, encodeMediaStatusReq, decodeMediaProgress, {
      uploadId,
    });
  }

  /**
   * Finalises an upload, making the object referenceable.
   *
   * The digest is the SHA-256 of the uploaded bytes; the server recomputes it over what it stored,
   * so a truncated or corrupted PUT is refused here rather than silently serving damaged media. On
   * success the server publishes a media-state event to the conversation. Resolves with nothing —
   * the caller already knows the id.
   */
  async commit(uploadId: Id, digest: Uint8Array): Promise<void> {
    await this.#rpc.call(OP.MEDIA_UPLOAD_COMMIT, encodeMediaCommit, decodeAcknowledged, {
      uploadId,
      digest,
    });
  }

  /**
   * Abandons an upload, telling the server to drop whatever bytes it holds for the ticket.
   *
   * Call it when a caller gives up mid-upload (or when {@link upload} fails on its behalf). An
   * abort of an unknown or already-committed ticket is an error the caller can ignore.
   */
  async abort(uploadId: Id): Promise<void> {
    await this.#rpc.call(OP.MEDIA_UPLOAD_ABORT, encodeMediaAbort, decodeAcknowledged, { uploadId });
  }

  /**
   * Requests a short-lived download URL for a committed object.
   *
   * The URL is membership-checked at issue time and expires on its own; `conversationId` scopes the
   * request when the caller holds a specific conversation view of the object. Request a fresh one
   * when {@link MediaUrl.expiresAt} passes rather than caching past the deadline.
   */
  async fetchUrl(objectId: Id, conversationId?: Id): Promise<MediaUrl> {
    const request: MediaFetch = { objectId };
    if (conversationId !== undefined) {
      request.conversationId = conversationId;
    }
    return this.#rpc.call(OP.MEDIA_FETCH_URL, encodeMediaFetch, decodeMediaUrl, request);
  }

  /**
   * Uploads a whole object in one call: begin, PUT, commit.
   *
   * The digest the commit needs is computed here, from the exact bytes sent. If any step after
   * {@link begin} fails the ticket is aborted (best-effort — the abort itself is not awaited into
   * the failure) so the server does not hold a half-written object, and the original error is
   * rethrown. A caller that wants to resume across flaky networks drives the steps itself instead.
   */
  async upload(options: UploadOptions, bytes: Uint8Array): Promise<UploadResult> {
    const ticket = await this.begin(options);
    try {
      await this.uploadBytes(ticket.uploadUrl, bytes);
      await this.commit(ticket.uploadId, await sha256(bytes));
    } catch (cause) {
      void this.abort(ticket.uploadId).catch(() => {
        // The abort is cleanup, not recovery: failing it must not mask the real error.
      });
      throw cause;
    }
    return { mediaId: ticket.uploadId };
  }

  /**
   * Resolves a fetchable URL for a committed object, for the caller to `fetch` itself.
   *
   * A convenience over {@link fetchUrl} that flattens the wire struct to the two fields a caller
   * acts on: the URL, and the moment it stops working.
   */
  async download(objectId: Id, conversationId?: Id): Promise<{ url: string; expiresAt: number }> {
    const granted = await this.fetchUrl(objectId, conversationId);
    return { url: granted.url, expiresAt: granted.expiresAt };
  }
}

/**
 * The SHA-256 of `bytes`, which is the digest {@link MediaDomain.commit} expects.
 *
 * Computed with WebCrypto, which every host this SDK runs in (browsers over HTTPS, Node 18+) ships;
 * an environment without it cannot commit an upload at all, so it fails loudly here rather than
 * sending a digest the server would reject.
 */
async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  if (globalThis.crypto?.subtle === undefined) {
    throw new SdkError('media: no WebCrypto implementation available to digest the upload');
  }
  // Same narrowing as `uploadBytes`: `digest` wants an ArrayBuffer-backed view.
  const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes as Uint8Array<ArrayBuffer>);
  return new Uint8Array(digest);
}
