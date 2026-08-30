/**
 * What the XP leaderboard is allowed to say about other people's standing.
 *
 * The leaderboard is a ranked projection of the economy domain's board read; its rows are the
 * same people the rest of the app shows, so two rules carry correctness weight:
 *
 *   1. **Rank is the server's, not the list's.** The position shown is the wire's `position`
 *      exactly — recomputing it from list order would drift the moment the server sent a page
 *      with a gap (a tie, a filtered account).
 *   2. **A row without a resolved profile keeps its rank.** The board names accounts; names and
 *      pictures are a profile-cache concern that may lag, and a lagging name must never cost
 *      the row its place.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, RankWire, UserProfile } from '@migo/sdk';

import { LeaderboardList } from '../src/components/gifts-panel.js';

const PROFILES = new Map<Id, UserProfile>([
  [
    'user_ada' as Id,
    { userId: 'user_ada' as Id, publicId: 'MGO-ADA', username: 'ada', displayName: 'Ada Lovelace' },
  ],
  [
    'user_grace' as Id,
    {
      userId: 'user_grace' as Id,
      publicId: 'MGO-GRACE',
      username: 'grace',
      displayName: 'Grace Hopper',
    },
  ],
]);

function rank(position: number, accountId: string, xp: number, level: number): RankWire {
  return { position, accountId: accountId as Id, xp, level };
}

test('each row shows the server\u2019s position, the person, their level, and their XP', () => {
  const markup = renderToStaticMarkup(
    <LeaderboardList
      ranks={[
        rank(1, 'user_ada', 5000, 9),
        rank(2, 'user_grace', 4100, 8),
        rank(3, 'user_alan', 3000, 7),
      ]}
      profiles={PROFILES}
    />,
  );

  assert.ok(markup.includes('#1'), 'the first position is missing');
  assert.ok(markup.includes('#2'), 'the second position is missing');
  assert.ok(markup.includes('#3'), 'the third position is missing');
  assert.ok(markup.includes('Ada Lovelace'), 'a resolved name is missing');
  assert.ok(markup.includes('Grace Hopper'), 'a resolved name is missing');
  assert.ok(markup.includes('Level 9'), 'a level line is missing');
  assert.ok(markup.includes('Level 8'), 'a level line is missing');
  assert.ok(markup.includes('5000 XP'), 'an XP line is missing');
  assert.ok(markup.includes('4100 XP'), 'an XP line is missing');
  // An account whose profile has not resolved keeps its ranked row under a stable fallback.
  assert.ok(markup.includes('Someone'), 'an unresolved account lost its ranked row');
});

test('the board\u2019s own positions are shown verbatim, never recomputed from order', () => {
  // A board page whose positions skip — a tie, a filtered account — must render the server's
  // numbers; a recomputed rank would quietly renumber the standings.
  const markup = renderToStaticMarkup(
    <LeaderboardList
      ranks={[rank(1, 'user_ada', 5000, 9), rank(4, 'user_grace', 4100, 8)]}
      profiles={PROFILES}
    />,
  );

  assert.ok(markup.includes('#1'), 'the first position is missing');
  assert.ok(markup.includes('#4'), 'the skipped position was renumbered away');
  assert.ok(!markup.includes('#2'), 'a position the server never stated was invented');
});

test('an empty board says so rather than rendering a hollow list', () => {
  const markup = renderToStaticMarkup(<LeaderboardList ranks={[]} profiles={new Map()} />);
  assert.ok(markup.includes('No one has earned XP yet.'));
});
