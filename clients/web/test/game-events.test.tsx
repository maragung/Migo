/**
 * What a game event row is allowed to say, and where it is allowed to say it.
 *
 * Game events are server-broadcast deltas rendered into a conversation transcript, and two
 * rules would silently regress under an innocent-looking refactor:
 *
 *   1. **A move line says only *that* somebody moved.** The published `moved` delta
 *      deliberately carries no board content (the server redacts per viewer, and the event is
 *      broadcast to every member), so a client that "helpfully" quoted a guess value or a board
 *      in the line would be inventing a disclosure the protocol declined to make. The test pins
 *      the line to the mover's name plus the game's, nothing more.
 *   2. **The guess input appears only when it is ours.** The active game's view carries the
 *      server-computed `yourTurn`; a client that gated the input on anything else (the game
 *      merely being open, the player list containing us) would render another member's solo
 *      game with our input wired to it — and our guesses rejected by a server that knows better.
 *
 * It also pins the guessing board's parser against the grammar the server's renderer writes
 * (`low-high:remaining` plus `guess:feedback` pairs), including its refusal to half-match, and
 * the rows' placement inside the message list's scrolling surface — a game line that rendered
 * outside the transcript would scroll away from the messages it belongs beside.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ContentType } from '@migo/sdk';
import type { GameViewWire, Id, UserProfile } from '@migo/sdk';

import { GameEventList } from '../src/components/game-events.js';
import { MessageList } from '../src/components/message-list.js';
import { gameEventLine, gameLabelOf, parseGuessBoard, playerRangeLabel } from '../src/lib/games.js';
import { appendRow, rowOf } from '../src/lib/migo/use-game-events.js';
import type { GameEventRow } from '../src/lib/migo/use-game-events.js';
import type { ThreadMessage } from '../src/lib/migo/use-chat.js';

const SELF = 'me' as Id;
const ADA = 'ada' as Id;
const GRACE = 'grace' as Id;

const PROFILES = new Map<Id, UserProfile>([
  [ADA, { userId: ADA, publicId: 'MGO-ADA', username: 'ada', displayName: 'Ada' }],
  [GRACE, { userId: GRACE, publicId: 'MGO-GRACE', username: 'grace', displayName: 'Grace' }],
]);

const GAME_ID = 'game_1' as Id;

/** An open guessing game viewed by its sole player. */
function guessView(overrides: Partial<GameViewWire> = {}): GameViewWire {
  return {
    gameId: GAME_ID,
    kind: 2,
    conversationId: 'conv_1' as Id,
    status: 0,
    players: [SELF],
    stateVersion: 3,
    board: '1-100:7 50:lower 25:higher',
    turnOf: SELF,
    yourTurn: true,
    ...overrides,
  };
}

function row(event: string, actorId?: Id, at = 1_000): GameEventRow {
  return {
    key: `k_${event}_${actorId ?? 'none'}`,
    gameId: GAME_ID,
    event,
    ...(actorId !== undefined ? { actorId } : {}),
    at,
  };
}

function render(
  rows: GameEventRow[],
  views: Map<Id, GameViewWire>,
  activeGuess: GameViewWire | null = null,
): string {
  return renderToStaticMarkup(
    <GameEventList
      rows={rows}
      views={views}
      selfId={SELF}
      profiles={PROFILES}
      activeGuess={activeGuess}
      onSubmitGuess={() => {}}
      guessBusy={false}
      guessError={null}
    />,
  );
}

test('a started game names the game and its players, ours included', () => {
  const view = guessView({ players: [SELF, ADA], kind: 0, board: '.........' });
  const line = gameEventLine(row('started'), view, SELF, PROFILES);
  assert.equal(line, '🎮 Tic-tac-toe started with You, Ada');
});

test('a moved line names the mover and the game, and never the move itself', () => {
  const line = gameEventLine(row('moved', ADA), guessView(), SELF, PROFILES);
  assert.equal(line, 'Ada made a move in Guess the number');
  // Our own move reads as ours, not in the third person.
  assert.equal(
    gameEventLine(row('moved', SELF), guessView(), SELF, PROFILES),
    'You made a move in Guess the number',
  );
  // The line for a game whose view has not arrived yet still names a game, generically.
  assert.equal(
    gameEventLine(row('moved', ADA), undefined, SELF, PROFILES),
    'Ada made a move in Game',
  );
});

test('a finished game announces the winner, or admits nobody won', () => {
  const view = guessView({ status: 1 });
  assert.equal(
    gameEventLine(row('finished', ADA), view, SELF, PROFILES),
    '🏆 Ada won Guess the number!',
  );
  assert.equal(
    gameEventLine(row('finished', SELF), view, SELF, PROFILES),
    '🏆 You won Guess the number!',
  );
  // A draw or a no-contest carries no actor: the line must not invent a winner.
  assert.equal(gameEventLine(row('finished'), view, SELF, PROFILES), 'Guess the number ended');
});

test('an event name this build does not know renders neutrally, never as the raw wire word', () => {
  assert.equal(
    gameEventLine(row('quantum_shift', ADA), guessView(), SELF, PROFILES),
    'Game update',
  );
});

test('game rows render as system lines, distinct from message bubbles, and a finish celebrates', () => {
  const views = new Map<Id, GameViewWire>([[GAME_ID, guessView()]]);
  const markup = render([row('started'), row('moved', ADA), row('finished', SELF)], views);
  assert.ok(markup.includes('class="game-event"'), 'a plain event row lost its styling');
  assert.ok(markup.includes('game-over'), 'a finished game must carry its celebration style');
  // The celebration row is still a centred system line, not a bubble: no bubble classes appear.
  assert.ok(!markup.includes('class="bubble'), 'a game row rendered as a message bubble');
});

test('the guess card offers the input only when the server says it is our turn', () => {
  const views = new Map<Id, GameViewWire>([[GAME_ID, guessView()]]);

  const ours = render([], views, guessView());
  assert.ok(
    ours.includes('placeholder="Enter your guess (1-100)"'),
    'the active game must offer the guess input',
  );
  assert.ok(ours.includes('guesses left'), 'the card must state the remaining guesses');
  assert.ok(ours.includes('The secret is higher.'), 'the card must quote the last feedback');

  // Another member's open solo game: visible rows are fine, but our input must not appear.
  const theirs = render([], new Map(), guessView({ players: [ADA], turnOf: ADA, yourTurn: false }));
  assert.ok(
    !theirs.includes('Enter your guess'),
    'another player\u2019s game offered us its input',
  );

  // A finished game offers no input, whoever played it.
  const over = render([], views, guessView({ status: 1 }));
  assert.ok(!over.includes('Enter your guess'), 'a finished game still offered an input');
});

test('the guess card reads its range and feedback from the board the server redacted', () => {
  const narrowing = guessView({ board: '25-50:5 60:lower' });
  const markup = render([], new Map(), narrowing);
  // The live range from the board, not the configured 1-100: the server narrowed it.
  assert.ok(markup.includes('25–50'), 'the card must show the board\u2019s live range');
  assert.ok(
    markup.includes('Enter your guess (25-50)'),
    'the input\u2019s bounds must follow the board, not a hard-coded range',
  );
  assert.ok(markup.includes('The secret is lower.'));
});

test('the guess card surfaces the submit flow\u2019s failure line', () => {
  const markup = renderToStaticMarkup(
    <GameEventList
      rows={[]}
      views={new Map()}
      selfId={SELF}
      profiles={PROFILES}
      activeGuess={guessView()}
      onSubmitGuess={() => {}}
      guessBusy={false}
      guessError={'The server rejected the request.'}
    />,
  );
  assert.ok(markup.includes('The server rejected the request.'));
});

test('an empty activity list renders nothing at all', () => {
  assert.equal(render([], new Map()), '');
});

test('the board parser reads the server\u2019s grammar and refuses to half-match', () => {
  const board = parseGuessBoard('1-100:6 50:lower 25:higher');
  assert.deepEqual(board, {
    low: 1,
    high: 100,
    remaining: 6,
    guesses: [
      { value: 50, feedback: 'lower' },
      { value: 25, feedback: 'higher' },
    ],
  });
  // A board with no guesses yet is still a board.
  assert.deepEqual(parseGuessBoard('1-100:7'), {
    low: 1,
    high: 100,
    remaining: 7,
    guesses: [],
  });
  // Anything else — another game's board, a grammar a newer server changed — is not ours to read.
  assert.equal(parseGuessBoard('X-vs-waiting'), null);
  assert.equal(parseGuessBoard('1-100:6 50:sideways'), null);
  assert.equal(parseGuessBoard(''), null);
});

test('the catalogue labels and player-range sentences match the server\u2019s vocabulary', () => {
  assert.equal(gameLabelOf(0), 'Tic-tac-toe');
  assert.equal(gameLabelOf(1), 'Rock paper scissors');
  assert.equal(gameLabelOf(2), 'Guess the number');
  // An unknown kind from a newer node stays generic rather than mis-named.
  assert.equal(gameLabelOf(99), 'Game');
  assert.equal(playerRangeLabel(1, 1), '1 player');
  assert.equal(playerRangeLabel(2, 2), '2 players');
  assert.equal(playerRangeLabel(2, 4), '2–4 players');
});

test('rows deduplicate by their wire identity and the newest are kept when the cap trims', () => {
  const base = {
    gameId: GAME_ID,
    roomId: 'conv_1' as Id,
    stateVersion: 3,
    event: 'moved',
    actorId: ADA,
  };
  const first = appendRow([], rowOf({ ...base }, 1));
  assert.equal(first.length, 1);
  // A redelivered event (the transport resuming its queue) is not a second row.
  assert.equal(appendRow(first, rowOf({ ...base }, 2)).length, 1);
  // A different state version is a different move.
  const second = appendRow(first, rowOf({ ...base, stateVersion: 4 }, 2));
  assert.equal(second.length, 2);
  // The cap keeps the newest tail.
  let rows = second;
  for (let version = 5; version < 80; version += 1) {
    rows = appendRow(rows, rowOf({ ...base, stateVersion: version }, version));
  }
  assert.ok(rows.length <= 60, 'the row cap did not hold');
  assert.equal(rows[rows.length - 1]?.key, rowOf({ ...base, stateVersion: 79 }, 79).key);
});

test('game rows render inside the message list\u2019s scrolling surface, below the messages', () => {
  const views = new Map<Id, GameViewWire>([[GAME_ID, guessView()]]);
  const message: ThreadMessage = {
    messageId: 'm1' as Id,
    conversationId: 'conv_1' as Id,
    seq: 1,
    senderId: ADA,
    senderDevice: 'd1' as Id,
    content: { type: ContentType.Text, text: 'your turn!' },
    createdAt: 1_000,
  };
  const markup = renderToStaticMarkup(
    <MessageList
      messages={[message]}
      selfId={SELF}
      showSenders={false}
      profiles={PROFILES}
      readUpTo={0}
      onReply={() => {}}
      onDelete={() => {}}
      deleting={false}
      hasEarlier={false}
      loadingEarlier={false}
      onLoadEarlier={() => {}}
      liveSlot={
        <GameEventList
          rows={[row('moved', ADA)]}
          views={views}
          selfId={SELF}
          profiles={PROFILES}
          activeGuess={null}
          onSubmitGuess={() => {}}
          guessBusy={false}
          guessError={null}
        />
      }
      liveRowCount={1}
    />,
  );
  const listAt = markup.indexOf('class="message-list"');
  const messageAt = markup.indexOf('your turn!');
  const gameAt = markup.indexOf('Ada made a move');
  const bottomAt = markup.lastIndexOf('<div');
  assert.ok(listAt >= 0 && messageAt > listAt && gameAt > messageAt, 'the game row is misplaced');
  assert.ok(gameAt < bottomAt, 'the game row must sit inside the scrolling surface');
});
