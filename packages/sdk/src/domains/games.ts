/**
 * The games domain: browse the catalogue, start a game, submit game actions, and observe game
 * state changes.
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
 *
 * # Why a move's result needs a second read
 *
 * `GAME_ACTION`'s reply is a bare ack and the published events say only *that* somebody moved, so
 * the substance of a move — feedback on a guess, a board — is fetched with {@link getView} after the
 * ack resolves. {@link startGame} answers with the full opening view directly, because a game that
 * has not started has no deltas to publish.
 */

import type { Id } from '@migo/wire';
import {
  OP,
  encodeGameAction,
  decodeGameEvent,
  decodeAcknowledged,
  encodeGameStart,
  decodeGameViewWire,
  encodeGameId,
  encodeGiftCatalogueReq,
  decodeGameCatalogueResponse,
} from '@migo/protocol';
import type {
  Acknowledged,
  GameAction,
  GameCatalogueEntry,
  GameEvent,
  GameId,
  GiftCatalogueReq,
  GameStart,
  GameViewWire,
} from '@migo/protocol';

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
   * Reads the node's game catalogue: one entry per kind it can referee.
   *
   * The catalogue is the node's own and versionless — the same posture as the gift catalogue — so
   * a client re-reads it to build its menu each session rather than caching it. Each entry carries
   * the slug {@link startGame} accepts and the player counts a client needs to know which games it
   * can even offer.
   */
  async getCatalogue(): Promise<GameCatalogueEntry[]> {
    const request: GiftCatalogueReq = {};
    const response = await this.#rpc.call(
      OP.GAME_CATALOGUE,
      encodeGiftCatalogueReq,
      decodeGameCatalogueResponse,
      request,
    );
    return response.games;
  }

  /**
   * Starts a game in a conversation and resolves with the opening view.
   *
   * `slug` is a catalogue entry's slug. The wire names no opponents, so in this build a start can
   * open the single-player guessing game and nothing else — the server refuses a multi-player kind
   * with "wrong number of players" rather than inventing an opponent on the caller's behalf, and
   * that refusal is surfaced here as a {@link RemoteError}. Nothing is published to the
   * conversation on start: the reply carries the opening view to the caller, and the other members
   * hear of the game when its first move publishes a {@link GameEvent}.
   */
  async startGame(conversationId: Id, slug: string): Promise<GameViewWire> {
    const request: GameStart = { conversationId, slug };
    return this.#rpc.call(OP.GAME_START, encodeGameStart, decodeGameViewWire, request);
  }

  /**
   * Reads one game's current view, as the caller is allowed to see it.
   *
   * The view is redacted per viewer by the server, so this is also how a player learns the outcome
   * of their own move: {@link submit}'s reply is a bare ack, and a move's substance — a guess's
   * higher/lower, a board — lives in the fresh view, not in the published events, which say only
   * *that* somebody moved.
   */
  async getView(gameId: Id): Promise<GameViewWire> {
    const request: GameId = { gameId };
    return this.#rpc.call(OP.GAME_VIEW, encodeGameId, decodeGameViewWire, request);
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
