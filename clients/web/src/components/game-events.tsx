'use client';

/**
 * Game activity rendered inside a conversation transcript.
 *
 * These rows are deliberately *not* message bubbles: a game event is not a message — it has no
 * sender the thread can reply to, no content to quote, and no delivery ticks. The row is a
 * centred, muted system line, the same visual family as a day divider, with one exception: a
 * finish gets the celebration treatment, because a win is the one game event a reader is waiting
 * to see.
 *
 * The guess card is the single interactive piece. It renders only for the active guessing game
 * while the server says it is the local player's turn — a solo game another member started is
 * theirs to play, and the card must not offer us their input — and its feedback line quotes the
 * board the server redacted for us, never a number computed locally.
 */

import { useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import type { GameViewWire, Id, UserProfile } from '@migo/sdk';

import {
  GAME_STATUS_OPEN,
  gameEventLine,
  gameLabelOf,
  guessFeedbackLine,
  parseGuessBoard,
} from '@/lib/games.js';
import type { GameEventRow } from '@/lib/migo/use-game-events.js';

export interface GameEventListProps {
  rows: GameEventRow[];
  views: ReadonlyMap<Id, GameViewWire>;
  selfId: Id;
  /** Resolved profiles, for player names in the lines. */
  profiles: ReadonlyMap<Id, UserProfile>;
  /**
   * The conversation's active guessing game, or null. The card still gates on the view's own
   * `yourTurn` and open status — another member's solo game is theirs to play, and a finished
   * game has no input left to offer.
   */
  activeGuess: GameViewWire | null;
  /** Submits a guess for the active game. */
  onSubmitGuess: (value: number) => void;
  guessBusy: boolean;
  guessError: string | null;
}

/** Renders the thread's game activity: one row per event, then the guess card when it is ours. */
export function GameEventList({
  rows,
  views,
  selfId,
  profiles,
  activeGuess,
  onSubmitGuess,
  guessBusy,
  guessError,
}: GameEventListProps): ReactNode {
  if (rows.length === 0 && activeGuess === null) {
    return null;
  }
  return (
    <div className="game-events" aria-label="Game activity">
      {rows.map((row) => (
        <div key={row.key} className={`game-event${row.event === 'finished' ? ' game-over' : ''}`}>
          {gameEventLine(row, views.get(row.gameId), selfId, profiles)}
        </div>
      ))}
      {activeGuess !== null &&
      activeGuess.yourTurn === true &&
      activeGuess.status === GAME_STATUS_OPEN ? (
        <GuessCard
          view={activeGuess}
          onSubmit={onSubmitGuess}
          busy={guessBusy}
          error={guessError}
        />
      ) : null}
    </div>
  );
}

/**
 * The inline guess input for the active game.
 *
 * Validation is local and lenient about *when* it blocks — the button disables until the field
 * holds an integer inside the board's live range — but the range itself is read from the board,
 * not hard-coded, so a server configured differently is obeyed rather than argued with. The
 * server re-validates regardless; an out-of-range value that slipped through comes back as its
 * own error line.
 */
function GuessCard({
  view,
  onSubmit,
  busy,
  error,
}: {
  view: GameViewWire;
  onSubmit: (value: number) => void;
  busy: boolean;
  error: string | null;
}): ReactNode {
  const [text, setText] = useState('');
  const board = parseGuessBoard(view.board);
  // Without a parsable board the card still offers the protocol's bound: the guessing game's
  // range is 1–100 by configuration, and a board this client cannot read is no reason to hide
  // the input the game is waiting on.
  const high = board?.high ?? 100;
  const low = board?.low ?? 1;
  const value = Number(text);
  const valid = Number.isInteger(value) && value >= low && value <= high;
  const feedback = board !== null ? guessFeedbackLine(board) : null;

  function onGuess(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    if (!valid || busy) {
      return;
    }
    onSubmit(value);
    setText('');
  }

  return (
    <form className="guess-card" onSubmit={onGuess}>
      <div className="guess-head">
        🎯 {gameLabelOf(view.kind)}
        {board !== null ? (
          <span className="guess-range">
            {board.low}–{board.high}, {board.remaining}{' '}
            {board.remaining === 1 ? 'guess' : 'guesses'} left
          </span>
        ) : null}
      </div>
      {/* The feedback line announces itself: it is the answer to the guess the reader just
          submitted, and a screen reader should hear it without hunting for it. */}
      <div className="guess-feedback" aria-live="polite">
        {feedback !== null ? feedback : null}
      </div>
      <div className="guess-controls">
        <input
          type="number"
          className="input"
          value={text}
          min={low}
          max={high}
          onChange={(event) => setText(event.target.value)}
          placeholder={`Enter your guess (${low}-${high})`}
          aria-label={`Enter your guess (${low} to ${high})`}
          disabled={busy}
        />
        <button type="submit" className="btn btn-primary" disabled={!valid || busy}>
          Guess
        </button>
      </div>
      {error !== null ? <div className="guess-error">{error}</div> : null}
    </form>
  );
}
