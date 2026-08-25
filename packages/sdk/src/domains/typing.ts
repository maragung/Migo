/**
 * The typing domain: publish this device's typing state, and observe others'.
 *
 * Typing is deliberately lossy. A {@link TypingEvent} has no reply and is coalescable — the server may
 * drop all but the latest for a conversation — because a typing indicator that arrives late is worse
 * than one that never arrives. So sending is fire-and-forget ({@link Rpc.notify}) and the client is
 * expected to debounce: emit {@link TypingState.Start} when the user begins, and {@link
 * TypingState.Stop} when they send or go idle, not one event per keystroke.
 *
 * Outbound events carry no `userId` — the server stamps it from the authenticated connection — so an
 * inbound event's `userId` identifies who is typing, and is always present on the events this domain
 * delivers.
 */

import type { Id } from '@migo/wire';
import { OP, TypingState, encodeTypingEvent, decodeTypingEvent } from '@migo/protocol';
import type { TypingEvent } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * Publish and observe typing indicators.
 *
 * One instance per client. Call {@link start} before {@link onTyping} handlers need to fire; sending
 * with {@link setTyping} works whether or not the inbound subscription is running.
 */
export class TypingDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<TypingEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.TYPING, onEventError);
  }

  /** Begins delivering inbound typing events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.TYPING, decodeTypingEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering inbound typing events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /** Registers a handler for inbound typing events. Returns an unsubscribe function. */
  onTyping(handler: Listener<TypingEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Publishes this device's typing state for a conversation.
   *
   * Fire-and-forget: the protocol defines no acknowledgement, and a dropped event is corrected by the
   * next state change. Debounce at the call site — a {@link TypingState.Start} when input begins and a
   * {@link TypingState.Stop} on send or idle, not one call per keystroke.
   */
  async setTyping(conversationId: Id, state: TypingState): Promise<void> {
    const event: TypingEvent = { conversationId, state };
    await this.#rpc.notify(OP.TYPING, encodeTypingEvent, event);
  }
}
