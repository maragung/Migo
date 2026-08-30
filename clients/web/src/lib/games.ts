/**
 * The games vocabulary a conversation's game UI speaks, as plain data.
 *
 * The wire carries game kinds and statuses as bare integers (the IDL defines no enums for them),
 * and the guessing game's board as one compact string. Everything a React component needs from
 * those — a label, a player-range sentence, the guesses with their feedback — is derived here, in
 * pure functions a test can pin, so the components stay markup and this module stays meaning.
 *
 * The integers are the server's own numbering (the games crate fixes them in code), mirrored here
 * rather than re-invented: an unknown value from a newer node still renders, under the generic
 * label, rather than being mis-named or dropped.
 */

import type { GameViewWire, Id, UserProfile } from '@migo/sdk';

import type { GameEventRow } from '@/lib/migo/use-game-events.js';

/** The kind numbers this build's server can referee (the games crate fixes them in code). */
export const GAME_KIND_TIC_TAC_TOE = 0;
export const GAME_KIND_ROCK_PAPER_SCISSORS = 1;
export const GAME_KIND_GUESS_NUMBER = 2;

/** The game statuses the store persists; `OPEN` is the only one a move may be applied to. */
export const GAME_STATUS_OPEN = 0;

/** The human label for a game kind; an unknown kind renders as a generic "Game". */
export function gameLabelOf(kind: number): string {
  switch (kind) {
    case GAME_KIND_TIC_TAC_TOE:
      return 'Tic-tac-toe';
    case GAME_KIND_ROCK_PAPER_SCISSORS:
      return 'Rock paper scissors';
    case GAME_KIND_GUESS_NUMBER:
      return 'Guess the number';
    default:
      return 'Game';
  }
}

/**
 * The player-count sentence for a catalogue entry, e.g. `1 player`, `2 players`, `2–4 players`.
 *
 * A range only reads as a range when the ends differ; a single-player game that said "1–1
 * players" would be arguing with itself.
 */
export function playerRangeLabel(minPlayers: number, maxPlayers: number): string {
  if (maxPlayers !== minPlayers) {
    return `${minPlayers}–${maxPlayers} players`;
  }
  return `${minPlayers} ${minPlayers === 1 ? 'player' : 'players'}`;
}

/** One parsed guess: the number guessed and what the server said about it. */
export interface GuessEntry {
  value: number;
  feedback: 'lower' | 'higher' | 'correct';
}

/** The guessing game's board line, parsed. The hidden number appears in no field, by design. */
export interface GuessBoard {
  low: number;
  high: number;
  remaining: number;
  guesses: GuessEntry[];
}

/**
 * Parses the guessing game's `board` string: `low-high:remaining` followed by one
 * ` guess:feedback` per guess, the feedback being `lower`, `higher`, or `correct`.
 *
 * Returns `null` for anything else — a board of a different game, or a grammar a newer server
 * changed — so a caller falls back to saying nothing rather than mis-quoting the state. The
 * parsing is strict on purpose: a number that "mostly" matched would invent a range the server
 * never stated.
 */
export function parseGuessBoard(board: string): GuessBoard | null {
  const parts = board.trim().split(/\s+/);
  const head = /^(\d+)-(\d+):(\d+)$/.exec(parts[0] ?? '');
  if (head === null) {
    return null;
  }
  const guesses: GuessEntry[] = [];
  for (const part of parts.slice(1)) {
    const entry = /^(\d+):(lower|higher|correct)$/.exec(part);
    if (entry === null) {
      return null;
    }
    guesses.push({ value: Number(entry[1]), feedback: entry[2] as GuessEntry['feedback'] });
  }
  return {
    low: Number(head[1]),
    high: Number(head[2]),
    remaining: Number(head[3]),
    guesses,
  };
}

/** The sentence the guess card shows about the newest guess, or null before the first one. */
export function guessFeedbackLine(board: GuessBoard): string | null {
  const last = board.guesses[board.guesses.length - 1];
  if (last === undefined) {
    return null;
  }
  switch (last.feedback) {
    case 'lower':
      return 'The secret is lower.';
    case 'higher':
      return 'The secret is higher.';
    case 'correct':
      return 'Correct!';
    default:
      return null;
  }
}

/**
 * The display name for an account in a game line: the profile's, "You" for ourselves, a stable
 * fallback otherwise. Game events name accounts the local profile cache may not hold yet (a
 * room member who has never spoken), and "Someone" is honest about that.
 */
function nameOf(id: Id | undefined, selfId: Id, profiles: ReadonlyMap<Id, UserProfile>): string {
  if (id === undefined) {
    return 'Someone';
  }
  if (id === selfId) {
    return 'You';
  }
  return profiles.get(id)?.displayName ?? 'Someone';
}

/**
 * The one-line text a game event row shows.
 *
 * The row's grammar is fixed per event name: a start names the game and its players, a move
 * names only the mover (the published delta deliberately says nothing *about* the move, and the
 * line must not either), a finish names the winner when there is one. An event name this build
 * does not know renders as a neutral "Game update" rather than the raw wire word, which is
 * server vocabulary a reader never chose to see.
 */
export function gameEventLine(
  row: GameEventRow,
  view: GameViewWire | undefined,
  selfId: Id,
  profiles: ReadonlyMap<Id, UserProfile>,
): string {
  const label = view !== undefined ? gameLabelOf(view.kind) : 'Game';
  switch (row.event) {
    case 'started': {
      const players =
        view !== undefined && view.players.length > 0
          ? view.players.map((player) => nameOf(player, selfId, profiles)).join(', ')
          : null;
      return players !== null ? `🎮 ${label} started with ${players}` : `🎮 ${label} started`;
    }
    case 'moved':
      return `${nameOf(row.actorId, selfId, profiles)} made a move in ${label}`;
    case 'turn_changed':
      return `${nameOf(row.actorId, selfId, profiles)}’s turn in ${label}`;
    case 'finished':
      // No actor on a finish means a draw or a no-contest: nobody won, so nobody is named.
      return row.actorId !== undefined
        ? `🏆 ${nameOf(row.actorId, selfId, profiles)} won ${label}!`
        : `${label} ended`;
    default:
      return 'Game update';
  }
}
