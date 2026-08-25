/**
 * The notifications domain: receive server-pushed notification events.
 *
 * These are the lightweight nudges a client turns into badges, banners, or OS notifications — a new
 * message in a muted-but-watched conversation, a mention, a friend request, a level-up. They are
 * receive-only: the server decides what to push and this domain only delivers it.
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
 * guaranteed one-to-one signal.
 */

import { OP, decodeNotificationEvent } from '@migo/protocol';
import type { NotificationEvent } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * Receive notification events.
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
}
