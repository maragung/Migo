/**
 * A small fan-out primitive shared by every event-driven domain.
 *
 * The subscribe-style domains — typing, presence, rooms, notifications, games — all do the same thing
 * with a server event: keep a set of application handlers, and when an event arrives, hand it to each.
 * The one subtlety is failure isolation. {@link Rpc.on} already routes a decode failure or a single
 * handler throw to the error sink, but it dispatches to the domain with *one* callback; if that
 * callback iterated the handlers itself and let one throw escape, the remaining handlers would be
 * starved of the event. So delivery is centralised here and each handler is invoked inside its own
 * `try`/`catch`, so a bug in one subscriber can never cost another subscriber its events.
 *
 * The messaging domain does not use this — it juggles three distinct listener kinds and an
 * out-of-order buffer, so its dispatch is bespoke — but every simpler domain does.
 */

import type { EventErrorHandler } from './rpc.js';

/** An application handler. Unsubscribed by calling the function {@link ListenerSet.add} returns. */
export type Listener<T> = (value: T) => void;

/**
 * A set of handlers for one event opcode, with per-handler failure isolation.
 *
 * A domain holds one of these per event it exposes, wires its {@link Rpc.on} subscription to
 * {@link deliver}, and hands callers {@link add} to register interest.
 */
export class ListenerSet<T> {
  readonly #opcode: number;
  readonly #onError: EventErrorHandler | undefined;
  readonly #listeners = new Set<Listener<T>>();

  /** `opcode` labels errors routed to `onError`; it identifies which event a failing handler was for. */
  constructor(opcode: number, onError?: EventErrorHandler) {
    this.#opcode = opcode;
    this.#onError = onError;
  }

  /** Registers a handler and returns a function that removes it. */
  add(listener: Listener<T>): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Whether any handler is currently registered, so a domain can skip work when nobody is listening. */
  get size(): number {
    return this.#listeners.size;
  }

  /** Delivers a value to every handler, isolating a throw from one so the rest still receive it. */
  deliver(value: T): void {
    for (const listener of this.#listeners) {
      try {
        listener(value);
      } catch (cause) {
        this.#onError?.(this.#opcode, cause);
      }
    }
  }
}
