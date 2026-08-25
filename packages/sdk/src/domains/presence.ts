/**
 * The presence domain: set this account's presence, and observe contacts' presence changes.
 *
 * Presence is a two-way channel: {@link setPresence} pushes this account's state (which the server
 * fans out to those allowed to see it), and {@link onPresence} receives others' changes. Visibility is
 * the server's decision, not this domain's — a client cannot see the presence of an account that has
 * not shared it, and {@link PresenceState.Invisible} lets an account appear offline while still
 * connected. This domain only sends its own state and renders what the server chooses to deliver.
 *
 * An inbound {@link PresenceEvent} may carry a `lastSeen` timestamp for an account that is now offline,
 * which is how a UI shows "last seen 5 minutes ago" without polling.
 */

import {
  OP,
  PresenceState,
  encodePresenceUpdate,
  decodePresenceEvent,
  decodeAcknowledged,
} from '@migo/protocol';
import type { PresenceUpdate, PresenceEvent, Acknowledged } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/** Optional detail attached to a presence update. */
export interface PresenceOptions {
  /** A free-text status line shown alongside the presence state, e.g. "in a meeting". */
  customStatus?: string;
}

/**
 * Publish and observe presence.
 *
 * One instance per client. {@link start} begins delivering inbound changes; {@link setPresence} works
 * independently of whether the inbound subscription is running.
 */
export class PresenceDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<PresenceEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.PRESENCE_EVENT, onEventError);
  }

  /** Begins delivering inbound presence events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.PRESENCE_EVENT, decodePresenceEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering inbound presence events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /** Registers a handler for inbound presence changes. Returns an unsubscribe function. */
  onPresence(handler: Listener<PresenceEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Sets this account's presence state.
   *
   * Resolves once the server has accepted the update. Use {@link PresenceState.Invisible} to stay
   * connected while appearing offline to others.
   */
  async setPresence(state: PresenceState, options: PresenceOptions = {}): Promise<Acknowledged> {
    const update: PresenceUpdate = { state };
    if (options.customStatus !== undefined) {
      update.customStatus = options.customStatus;
    }
    return this.#rpc.call(OP.PRESENCE_SET, encodePresenceUpdate, decodeAcknowledged, update);
  }
}
