/**
 * The notifications domain: the push inbox and its read state.
 *
 * These are the lightweight nudges a client turns into badges, banners, or OS notifications — a new
 * message in a muted-but-watched conversation, a mention, a friend request, a level-up. The live
 * half ({@link NotificationEvent}) is receive-only: the server decides what to push and this domain
 * only delivers it. The persisted half is the *inbox* — the same nudges as rows that survive the
 * recipient being offline — read here ({@link listNotifications}) and marked read
 * ({@link acknowledgeNotifications}).
 *
 * # No private plaintext, ever
 *
 * A {@link NotificationEvent} deliberately carries no message content. It names a {@link
 * NotificationKind} and points at a conversation, room, or actor by id, so the client can fetch and
 * decrypt the real content itself if it chooses. Any `title` or `body` is server-composable metadata
 * (a room name, a sender's display name), never the plaintext of a private message — the server has no
 * plaintext to put there. A client rendering an OS notification for a message therefore shows "New
 * message" or the sender's name, and reveals the text only after decrypting it locally.
 *
 * The stream is droppable: under load the server may shed these before anything that carries state, so
 * a client treats a missed notification as a cue to reconcile (sync, re-count unreads), not as a
 * guaranteed one-to-one signal. The inbox is the durable complement — when the bell says something
 * arrived but no event did, the row is still there to be listed.
 */

import type { Id } from '@migo/wire';
import { idFromBytes } from '@migo/wire';
import {
  OP,
  encodeInboxReq,
  decodeInboxResponse,
  encodeNotificationAck,
  decodeAcknowledged,
  decodeNotificationEvent,
} from '@migo/protocol';
import type { InboxItem, InboxReq, NotificationAck, NotificationEvent } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/** The inbox page {@link NotificationsDomain.listNotifications} asks for by default. */
const DEFAULT_INBOX_LIMIT = 50;

/**
 * Receive notification events, read the inbox, and acknowledge it.
 *
 * One instance per client. Nothing is delivered until {@link start}, so a client registers its handler
 * first and does not miss the first push.
 */
export class NotificationsDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<NotificationEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.NOTIFICATION_EVENT, onEventError);
  }

  /** Begins delivering inbound notification events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.NOTIFICATION_EVENT, decodeNotificationEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering notification events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /** Registers a handler for inbound notifications. Returns an unsubscribe function. */
  onNotification(handler: Listener<NotificationEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Reads one page of the caller's inbox, newest first.
   *
   * The server keeps no pagination cursor for the inbox: `nextCursor` is always absent and a client
   * pages by re-asking with a higher limit. Rows arrive as {@link InboxItem}s — kind, timestamp, and
   * the ids the kind points at — carrying no message content, per the no-plaintext rule above.
   */
  async listNotifications(limit: number = DEFAULT_INBOX_LIMIT): Promise<InboxItem[]> {
    const request: InboxReq = { limit };
    const response = await this.#rpc.call(
      OP.NOTIFICATION_LIST,
      encodeInboxReq,
      decodeInboxResponse,
      request,
    );
    return response.items;
  }

  /**
   * Marks every notification at or before one instant as read.
   *
   * `through` is a Unix-millisecond watermark, normally the `at` of the newest item the caller has
   * rendered (from {@link listNotifications}) — the "I have opened the bell" gesture. The wire
   * carries a single notification id rather than a timestamp, and the server reads the id's embedded
   * time prefix as the watermark, so this synthesises an id whose prefix *is* `through`; one call
   * then clears the named instant and everything older, and a notification landing mid-flight is
   * simply left for the next ack rather than raced.
   */
  async acknowledgeNotifications(through: number): Promise<void> {
    const request: NotificationAck = { id: watermarkId(through) };
    await this.#rpc.call(OP.NOTIFICATION_ACK, encodeNotificationAck, decodeAcknowledged, request);
  }
}

/**
 * The id whose time prefix is `unixMs`: six big-endian bytes of it, then zeros.
 *
 * Only the prefix is ever read server-side ({@link NotificationsDomain.acknowledgeNotifications}),
 * so the random tail a real id carries is replaced with zeros — this value names an instant, not an
 * entity, and must not be mistaken for a persisted notification's id.
 */
function watermarkId(unixMs: number): Id {
  const bytes = new Uint8Array(16);
  let ms = unixMs;
  for (let i = 5; i >= 0; i -= 1) {
    bytes[i] = ms & 0xff;
    ms = Math.floor(ms / 256);
  }
  return idFromBytes(bytes);
}
