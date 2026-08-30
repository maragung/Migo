'use client';

/**
 * Live game activity for one conversation.
 *
 * Games surface in a thread as two different things, and this hook owns both:
 *
 *   1. **Rows** — the {@link GameEvent} deltas the server publishes to the conversation after
 *      every move (and, in builds that publish it, on start). They are a live stream, not part
 *      of the message history: the sync replay carries no game events, so a freshly opened
 *      thread starts with no rows and grows them as moves happen. Rows are capped, newest kept.
 *   2. **Views** — the per-viewer {@link GameViewWire} a client must fetch to render anything
 *      with substance (a game's name, its players, a guess's feedback). Events say only *that*
 *      somebody moved; the view is where the board lives. The hook fetches a view for a game it
 *      has not seen and refreshes it when an event says the board changed.
 *
 * The one flow that stitches both together is starting a game: `GAME_START` publishes nothing
 * (its reply *is* the announcement, to the caller alone), so {@link startGame} synthesizes the
 * "started" row locally from the reply's view. Other members hear of the game when its first
 * move publishes real events.
 *
 * The guessing game's feedback needs one more read: `GAME_ACTION`'s reply is a bare ack, so
 * {@link submitGuess} re-fetches the view after the ack to learn the higher/lower/correct the
 * fresh board carries.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import type { GameEvent, GameViewWire, Id } from '@migo/sdk';

import { GAME_KIND_GUESS_NUMBER, GAME_STATUS_OPEN } from '@/lib/games.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

/** How many activity rows to keep; the stream is unbounded, the transcript is not. */
const MAX_ROWS = 60;

/**
 * One rendered line of game activity: a published {@link GameEvent}, or the local start
 * acknowledgement the server never publishes on our behalf.
 *
 * `key` deduplicates: a redelivered event (the transport resuming its queue) must not become a
 * second row, and the key is built from everything the wire puts on the line.
 */
export interface GameEventRow {
  key: string;
  gameId: Id;
  /** The event name the server publishes: `started`, `moved`, `turn_changed`, `finished`. */
  event: string;
  /** Who the event is about: the mover, the player whose turn it now is, the winner. */
  actorId?: Id;
  /** Arrival time, for ordering only — the wire carries no game clock a client may quote. */
  at: number;
  /** Set on the row synthesized from our own start reply, which the server does not publish. */
  local?: boolean;
}

/** Builds the row for a published event, with the deduplication key the wire determines. */
export function rowOf(event: GameEvent, at: number = Date.now()): GameEventRow {
  return {
    key: `${event.gameId}:${event.stateVersion}:${event.event}:${event.actorId ?? ''}`,
    gameId: event.gameId,
    event: event.event,
    ...(event.actorId !== undefined ? { actorId: event.actorId } : {}),
    at,
  };
}

/** Appends a row, dropping a duplicate key and trimming to the newest {@link MAX_ROWS}. */
export function appendRow(rows: GameEventRow[], row: GameEventRow): GameEventRow[] {
  if (rows.some((existing) => existing.key === row.key)) {
    return rows;
  }
  const next = [...rows, row];
  return next.length > MAX_ROWS ? next.slice(next.length - MAX_ROWS) : next;
}

/** A game view plus when this session first noted it, so "the active game" is well-defined. */
interface NotedView {
  view: GameViewWire;
  notedAt: number;
}

export interface GameActivity {
  /** Activity rows in arrival order — the live tail of the thread's game traffic. */
  rows: GameEventRow[];
  /** The views fetched for the games seen in this conversation, keyed by game id. */
  views: ReadonlyMap<Id, GameViewWire>;
  /**
   * The conversation's newest open guessing game, when there is one. Rendering the guess input
   * is still gated on `yourTurn` — another member's solo game is theirs to play, not ours.
   */
  activeGuess: GameViewWire | null;
  /** Starts a game from the catalogue and synthesizes its "started" row. Rejects on refusal. */
  startGame: (slug: string) => Promise<GameViewWire>;
  /** Submits a guess for the active game, then re-reads the view that carries the feedback. */
  submitGuess: (value: number) => Promise<void>;
  guessBusy: boolean;
  guessError: string | null;
}

export function useGameEvents(conversationId: Id): GameActivity {
  const { client, resetNonce } = useMigo();

  const [rows, setRows] = useState<GameEventRow[]>([]);
  const [views, setViews] = useState<ReadonlyMap<Id, GameViewWire>>(new Map());
  const [guessBusy, setGuessBusy] = useState(false);
  const [guessError, setGuessError] = useState<string | null>(null);

  // Mirrors for async contexts: a view fetch resolving after a conversation switch must not
  // write a stale game's state into the new thread.
  const viewsRef = useRef(new Map<Id, NotedView>());
  const fetching = useRef(new Set<Id>());

  const noteView = useCallback((view: GameViewWire): void => {
    viewsRef.current.set(view.gameId, { view, notedAt: Date.now() });
    setViews(new Map(notedViews(viewsRef.current)));
  }, []);

  /**
   * Best-effort view refresh, coalesced per game: one move publishes several events at one
   * state version, and they must cost one read, not one per event. A failure is swallowed —
   * the rows still describe the move, and the next event retries the read.
   */
  const refreshView = useCallback(
    (gameId: Id): void => {
      if (!client || fetching.current.has(gameId)) {
        return;
      }
      fetching.current.add(gameId);
      client.games
        .getView(gameId)
        .then(noteView)
        .catch(() => {})
        .finally(() => {
          fetching.current.delete(gameId);
        });
    },
    [client, noteView],
  );

  // The live event stream for this conversation. A reset rebuilds the world: rows and views are
  // session-scoped state, and the server's post-reset replay carries no game events to repopulate
  // them from, so they start empty rather than half-remembered.
  useEffect(() => {
    if (!client) {
      return;
    }
    setRows([]);
    viewsRef.current = new Map();
    setViews(new Map());

    const off = client.games.onGameEvent((event) => {
      // The wire field is named `roomId`, but the server publishes the *conversation* id there —
      // one subject, two names — so this is the thread's own filter.
      if (event.roomId !== conversationId) {
        return;
      }
      setRows((prev) => appendRow(prev, rowOf(event)));
      const held = viewsRef.current.has(event.gameId);
      // `moved` and `finished` mean the board changed under the view we hold; an unknown game
      // needs its first read to render anything but a nameless line.
      if (event.event === 'moved' || event.event === 'finished' || !held) {
        refreshView(event.gameId);
      }
    });
    return off;
  }, [client, conversationId, resetNonce, refreshView]);

  // The active guessing game: the newest open one this session has noted. Computed on each
  // render from the ref (commit updates it before the state that triggers the render), so
  // "newest" follows the most recently touched game — the one whose input the reader is
  // waiting on.
  const activeGuess = newestOpenGuess(viewsRef.current);

  const startGame = useCallback(
    async (slug: string): Promise<GameViewWire> => {
      if (!client) {
        throw new Error('not connected');
      }
      // The reply is the opening view; the server publishes nothing on start, so the "started"
      // row is this client's own synthesis, marked local to make its provenance legible.
      const view = await client.games.startGame(conversationId, slug);
      noteView(view);
      setRows((prev) =>
        appendRow(prev, {
          key: `local:${view.gameId}`,
          gameId: view.gameId,
          event: 'started',
          at: Date.now(),
          local: true,
        }),
      );
      return view;
    },
    [client, conversationId, noteView],
  );

  const submitGuess = useCallback(
    async (value: number): Promise<void> => {
      const game = newestOpenGuess(viewsRef.current);
      if (!client || game === null || guessBusy) {
        return;
      }
      setGuessBusy(true);
      setGuessError(null);
      try {
        await client.games.submit(game.gameId, conversationId, 'guess', {
          args: [String(value)],
        });
        // The ack carries nothing; the feedback (higher/lower/correct) is in the fresh board.
        // A failed re-read leaves the row stream to say the move happened; only the hint line
        // waits for the next refresh.
        try {
          noteView(await client.games.getView(game.gameId));
        } catch {
          // The move itself was accepted; the feedback line catches up on the next event.
        }
      } catch (cause) {
        setGuessError(friendlyError(cause));
      } finally {
        setGuessBusy(false);
      }
    },
    [client, conversationId, guessBusy, noteView],
  );

  return {
    rows,
    views,
    activeGuess,
    startGame,
    submitGuess,
    guessBusy,
    guessError,
  };
}

/** Projects the noted views onto the plain map the render path reads. */
function notedViews(noted: Map<Id, NotedView>): Map<Id, GameViewWire> {
  const out = new Map<Id, GameViewWire>();
  for (const [gameId, entry] of noted) {
    out.set(gameId, entry.view);
  }
  return out;
}

/** The newest open guessing game among the noted views, or null when there is none. */
function newestOpenGuess(noted: Map<Id, NotedView>): GameViewWire | null {
  let active: GameViewWire | null = null;
  let activeAt = -1;
  for (const entry of noted.values()) {
    if (entry.view.kind !== GAME_KIND_GUESS_NUMBER || entry.view.status !== GAME_STATUS_OPEN) {
      continue;
    }
    if (entry.notedAt > activeAt) {
      active = entry.view;
      activeAt = entry.notedAt;
    }
  }
  return active;
}
