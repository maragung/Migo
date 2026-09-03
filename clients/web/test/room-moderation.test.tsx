/**
 * What a room's moderation controls are allowed to do, and to whom.
 *
 * The panel hands two recourses to the roster, and each is gated by a pure predicate this file
 * pins — because a regression here is not a broken render but a silent authority bug: a control
 * that appears where it must not (a kick button on the owner, a ban a plain member should never
 * see) or vanishes where it belongs.
 *
 *   1. **A kick vote is everyone's, save two rows.** {@link canVoteKick} offers it on every other
 *      member — never your own row, never the owner, whom a show of hands cannot unseat.
 *   2. **A sanction respects rank, and never the owner.** {@link canSanction} admits a moderator or
 *      above acting strictly below their own rank, and a global admin acting on any non-owner
 *      member; it refuses a peer of equal rank, anyone higher, and the owner outright.
 *   3. **The tally reads as a fraction.** {@link voteTally} shows votes over the count needed, and
 *      degrades to a bare count when the room has stated no threshold.
 *   4. **The list renders exactly the controls the predicates admit.** A moderator viewing the
 *      roster sees the staff row on a lower-ranked member and nothing on the owner or their own row.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RoomRole } from '@migo/sdk';
import type { Id, RosterEntry } from '@migo/sdk';

import {
  RosterList,
  canSanction,
  canVoteKick,
  voteTally,
} from '../src/components/room-info-panel.js';

const JOINED = Date.parse('2026-08-26T12:00:00Z');

function roster(accountId: string, role: number): RosterEntry {
  return { accountId: accountId as Id, role, joinedAt: JOINED };
}

test('a kick-vote tally reads as a fraction, and degrades to a bare count without a threshold', () => {
  assert.equal(voteTally(3, 17), '3/17');
  assert.equal(voteTally(0, 4), '0/4');
  // No threshold stated yet (a room too small, or before the first vote lands): the count stands
  // alone rather than dividing by nothing.
  assert.equal(voteTally(2, 0), '2');
  assert.equal(voteTally(1, -1), '1');
  // A stale negative never renders below zero.
  assert.equal(voteTally(-5, 10), '0/10');
});

test('the kick vote is offered on every member but yourself and the owner', () => {
  // Not your own row, whatever your rank.
  assert.equal(canVoteKick(RoomRole.Member, true), false);
  assert.equal(canVoteKick(RoomRole.Owner, true), false);
  // Not the owner's row, whoever asks.
  assert.equal(canVoteKick(RoomRole.Owner, false), false);
  // Everyone else is fair game — a plain member, a helper, even a moderator or admin.
  assert.equal(canVoteKick(RoomRole.Member, false), true);
  assert.equal(canVoteKick(RoomRole.Helper, false), true);
  assert.equal(canVoteKick(RoomRole.Moderator, false), true);
  assert.equal(canVoteKick(RoomRole.Admin, false), true);
});

test('a sanction respects rank, spares the owner, and bows to a global admin', () => {
  // The owner is untouchable from this panel, by anyone — moderator, admin, even a global admin.
  assert.equal(canSanction(RoomRole.Admin, RoomRole.Owner, false), false);
  assert.equal(canSanction(RoomRole.Moderator, RoomRole.Owner, false), false);
  assert.equal(canSanction(RoomRole.Member, RoomRole.Owner, true), false);
  // A moderator acts strictly below their own rank: yes on a member or helper...
  assert.equal(canSanction(RoomRole.Moderator, RoomRole.Member, false), true);
  assert.equal(canSanction(RoomRole.Moderator, RoomRole.Helper, false), true);
  // ...no on a peer of equal rank, and none up the ladder.
  assert.equal(canSanction(RoomRole.Moderator, RoomRole.Moderator, false), false);
  assert.equal(canSanction(RoomRole.Moderator, RoomRole.Admin, false), false);
  // A plain member has no staff controls at all.
  assert.equal(canSanction(RoomRole.Member, RoomRole.Member, false), false);
  assert.equal(canSanction(RoomRole.Member, RoomRole.Helper, false), false);
  // A global admin acts on any non-owner member, whatever room rank they hold (here, little or none).
  assert.equal(canSanction(RoomRole.Member, RoomRole.Admin, true), true);
  assert.equal(canSanction(RoomRole.Unknown, RoomRole.Moderator, true), true);
});

test('a moderator viewing the roster sees the staff controls on a member, and nothing on the owner or on their own row', () => {
  const markup = renderToStaticMarkup(
    <RosterList
      entries={[
        roster('user_owner', RoomRole.Owner),
        roster('user_me', RoomRole.Moderator),
        roster('user_bob', RoomRole.Member),
      ]}
      profiles={
        new Map([
          ['user_owner' as Id, { displayName: 'Ada' }],
          ['user_me' as Id, { displayName: 'Me' }],
          ['user_bob' as Id, { displayName: 'Bob' }],
        ])
      }
      viewerId={'user_me' as Id}
      viewerRole={RoomRole.Moderator}
      tallies={new Map([['user_bob' as Id, '3/17']])}
      onVoteKick={() => {}}
      onRoomMute={() => {}}
      onKick={() => {}}
      onBan={() => {}}
    />,
  );

  // The member below the moderator carries the whole toolset: a vote, a room silence, a kick, a ban.
  assert.ok(markup.includes('Vote kick'), 'a member lost the kick-vote control');
  assert.ok(markup.includes('Silence in room'), 'a member lost the room-silence control');
  assert.ok(markup.includes('>Kick<'), 'a member lost the kick control');
  assert.ok(markup.includes('>Ban<'), 'a member lost the ban control');
  // The live tally rides on the member's row.
  assert.ok(markup.includes('Vote to kick: 3/17'), 'the live kick-vote tally is missing');
  // Exactly one row bears each staff control (Bob's) — not the owner's, not the moderator's own.
  assert.equal(
    (markup.match(/Silence in room/g) ?? []).length,
    1,
    'the room-silence control leaked onto a row it must not touch',
  );
  assert.equal((markup.match(/>Ban</g) ?? []).length, 1, 'the ban control leaked onto another row');
  // The kick vote appears once too: on Bob, never on the owner (a vote cannot unseat one) nor on
  // the viewer's own row.
  assert.equal(
    (markup.match(/Vote kick/g) ?? []).length,
    1,
    'the kick vote appeared on the owner or the viewer’s own row',
  );
});

test('with no viewer context the roster is a plain read, offering no controls at all', () => {
  const markup = renderToStaticMarkup(
    <RosterList
      entries={[roster('user_bob', RoomRole.Member)]}
      profiles={new Map([['user_bob' as Id, { displayName: 'Bob' }]])}
    />,
  );
  assert.ok(markup.includes('Bob'), 'the member row is missing');
  assert.ok(!markup.includes('Vote kick'), 'a control appeared without a viewer to authorise it');
  assert.ok(!markup.includes('Silence in room'), 'a staff control appeared on a plain read');
});
