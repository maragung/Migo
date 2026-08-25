/**
 * The games domain: submit game actions, and observe game state changes.
 *
 * Games are room-scoped and server-authoritative. A client never computes an outcome; it submits an
 * *intent* ({@link GameAction}) — "I play this card", "I roll" — and the server decides what happens
 * and broadcasts the result as a {@link GameEvent}. This split is deliberate (brief section 89): a
 * client cannot fabricate a win, because the client that submits the action is not the party that
 * rules on it. Rewards are virtual and non-monetary throughout (sections 37 and 87); there is no
 * real-money stake anywhere in this path.
 *
 * # Actions are deduplicated, events are deltas
 *
 * Each action carries an `actionId` that is monotonic per game for this player, so a resubmitted
 * action (a retry after a flaky connection) is recognised as the same one and not applied twice. This
 * domain mints that id from an in-memory per-game counter, so ordinary use never has to manage it.
 *
 * A {@link GameEvent} is a delta, never a full snapshot: it names an `event` and an optional binary
 * `payload` describing the change, plus a `stateVersion` a client uses to detect a missed event and
 * resync. A pre-rendered `text` line is included for thin clients that render the game as chat rather
 * than interpreting the payload.
 */

import type { Id } from '@migo/wire';
import { OP, encodeGameAction, decodeGameEvent, decodeAcknowledged } from '@migo/protocol';
import type { GameAction, GameEvent, Acknowledged } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * Submit game actions and observe game events.
 *
 * One instance per client. {@link start} begins delivering events; {@link submit} works independently.
 */
export class GamesDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<GameEvent>;
  /** The next action id to mint per game, so retries reuse an id and the server dedupes them. */
  readonly #nextActionId = new Map<Id, number>();
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.GAME_EVENT, onEventError);
  }

  /** Begins delivering inbound game events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.GAME_EVENT, decodeGameEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering game events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /** Registers a handler for inbound game events. Returns an unsubscribe function. */
  onGameEvent(handler: Listener<GameEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Submits a game action as an intent; the server decides and broadcasts the outcome.
   *
   * The `actionId` is minted automatically and monotonically per game, so submitting the "same" action
   * twice (a deliberate retry) would carry the next id and be treated as a new action — for an
   * idempotent retry, pass the previous id explicitly as {@link SubmitOptions.actionId}. Resolves once
   * the server has accepted the intent; the resulting state change arrives separately as a {@link
   * GameEvent}.
   */
  async submit(
    gameId: Id,
    roomId: Id,
    action: string,
    options: SubmitOptions = {},
  ): Promise<Acknowledged> {
    const actionId = options.actionId ?? this.#mintActionId(gameId);
    const request: GameAction = { gameId, roomId, actionId, action };
    if (options.args !== undefined) {
      request.args = options.args;
    }
    return this.#rpc.call(OP.GAME_ACTION, encodeGameAction, decodeAcknowledged, request);
  }

  /** Returns the next monotonic action id for a game, advancing the per-game counter. */
  #mintActionId(gameId: Id): number {
    const next = this.#nextActionId.get(gameId) ?? 1;
    this.#nextActionId.set(gameId, next + 1);
    return next;
  }
}

/** Optional parameters for {@link GamesDomain.submit}. */
export interface SubmitOptions {
  /** Arguments for the action, e.g. the coordinates of a move. */
  args?: string[];
  /**
   * An explicit action id, overriding the auto-minted one.
   *
   * Pass the id of a previous submission to retry it idempotently — the server recognises the repeated
   * id and does not apply the action a second time.
   */
  actionId?: number;
}
