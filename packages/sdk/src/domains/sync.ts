/**
 * The sync domain: fetch conversation history the client is missing.
 *
 * A client holds a highest-contiguous sequence number per conversation. When it reconnects, or when a
 * live {@link MessageEvent} arrives with a `seq` past that watermark (a gap), it asks the server for
 * the range in between. The returned {@link MessageEvent}s are still sealed — sync moves ciphertext,
 * not plaintext, because the server has no plaintext to move — so the caller replays each through
 * {@link MessagingDomain.ingest}, which decrypts and routes it exactly as a live delivery.
 *
 * # Why replay order matters
 *
 * History is returned in ascending sequence order, and a client must ingest it in that order. A
 * sender-key distribution ({@link MessageKind.KeyExchange}) has a lower seq than the content it
 * unlocks, so replaying forward hands the messaging layer the key before the messages that need it. A
 * ranged or backwards fetch can still deliver content before its key; the messaging layer's bounded
 * per-sender buffer absorbs that and drains when the distribution replays.
 *
 * # The truncation boundary
 *
 * The server may cap how far back it will serve. A {@link SyncStatus.Truncated} response means older
 * history exists but was not returned, so the UI should render a "messages before here are
 * unavailable" boundary rather than silently presenting a gap as continuity.
 */

import type { Id } from '@migo/wire';
import { OP, encodeSyncRequest, decodeSyncResponse } from '@migo/protocol';
import type { SyncRequest, SyncResponse } from '@migo/protocol';

import type { Rpc } from './rpc.js';

/** Optional bounds on a sync fetch beyond "everything after `haveSeq`". */
export interface SyncOptions {
  /** Fetch up to this sequence number, to fill a detected gap rather than tail the latest. */
  toSeq?: number;
  /** Page older history below `haveSeq` instead of newer history above it. */
  backwards?: boolean;
}

/**
 * Fetch missing conversation history.
 *
 * One instance per client. Holds no watermark itself — the caller tracks the highest contiguous seq
 * it has and passes it as `haveSeq` — so this domain stays a stateless request wrapper.
 */
export class SyncDomain {
  readonly #rpc: Rpc;

  constructor(rpc: Rpc) {
    this.#rpc = rpc;
  }

  /**
   * Fetches up to `limit` messages for a conversation.
   *
   * `haveSeq` is the highest contiguous sequence the caller already holds; the server returns messages
   * after it (or, with {@link SyncOptions.backwards}, before it). The caller replays each message in
   * {@link SyncResponse.messages} through {@link MessagingDomain.ingest} in order, then advances its
   * watermark to {@link SyncResponse.toSeq}. A {@link SyncResponse.more} of `true` means another page
   * is available with the returned `toSeq` as the next `haveSeq`.
   */
  async fetch(
    conversationId: Id,
    haveSeq: number,
    limit: number,
    options: SyncOptions = {},
  ): Promise<SyncResponse> {
    const request: SyncRequest = { conversationId, haveSeq, limit };
    if (options.toSeq !== undefined) {
      request.toSeq = options.toSeq;
    }
    if (options.backwards !== undefined) {
      request.backwards = options.backwards;
    }
    return this.#rpc.call(OP.SYNC, encodeSyncRequest, decodeSyncResponse, request);
  }
}
