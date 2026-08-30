/**
 * What a room's roster is allowed to say about who is in it.
 *
 * The roster arrives as wire rows whose `role` is a plain number; the label mapping is the one
 * piece of vocabulary the panel owns, so it is pinned as a function and the list's rendering is
 * pinned as markup:
 *
 *   1. **Roles collapse to the three words a reader needs.** Owner is Owner, the staff ranks
 *      (Admin, Manager) are Admin, everything else — Member, Helper, Moderator, and any value a
 *      newer node sent — is Member. A guessed label for an unknown role would invent a rank the
 *      server never stated.
 *   2. **The roster shows people, not account ids.** Names resolve through the profile map; an
 *      unresolved account keeps a stable "Someone" fallback rather than a blank row.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RoomRole } from '@migo/sdk';
import type { Id, RosterEntry } from '@migo/sdk';

import { RosterList, roleLabel } from '../src/components/room-info-panel.js';

const JOINED = Date.parse('2026-08-26T12:00:00Z');

function roster(accountId: string, role: number): RosterEntry {
  return { accountId: accountId as Id, role, joinedAt: JOINED };
}

test('roster roles collapse to Owner, Admin, and Member', () => {
  assert.equal(roleLabel(RoomRole.Owner), 'Owner');
  assert.equal(roleLabel(RoomRole.Admin), 'Admin');
  assert.equal(roleLabel(RoomRole.Manager), 'Admin');
  assert.equal(roleLabel(RoomRole.Member), 'Member');
  assert.equal(roleLabel(RoomRole.Helper), 'Member');
  assert.equal(roleLabel(RoomRole.Moderator), 'Member');
  // Unknown = 0 and any value a newer node sent: Member, never a guess at a rank.
  assert.equal(roleLabel(RoomRole.Unknown), 'Member');
  assert.equal(roleLabel(99), 'Member');
});

test('the roster list renders one labelled row per member, with roles as badges', () => {
  const markup = renderToStaticMarkup(
    <RosterList
      entries={[
        roster('user_ada', RoomRole.Owner),
        roster('user_grace', RoomRole.Admin),
        roster('user_alan', RoomRole.Member),
        roster('user_ghost', RoomRole.Unknown),
      ]}
      profiles={
        new Map([
          ['user_ada' as Id, { displayName: 'Ada Lovelace' }],
          ['user_grace' as Id, { displayName: 'Grace Hopper' }],
          ['user_alan' as Id, { displayName: 'Alan Turing' }],
        ])
      }
    />,
  );

  assert.ok(markup.includes('Ada Lovelace'), 'a member\u2019s resolved name is missing');
  assert.ok(markup.includes('Grace Hopper'), 'a member\u2019s resolved name is missing');
  assert.ok(markup.includes('Alan Turing'), 'a member\u2019s resolved name is missing');
  // An unresolved account keeps its row: the rank is the fact, the name is its label.
  assert.ok(markup.includes('Someone'), 'an unresolved member lost their row');
  assert.ok(markup.includes('role-owner'), 'the owner badge lost its distinguishing style');
  assert.ok(markup.includes('role-admin'), 'the admin badge lost its distinguishing style');
  assert.equal(
    (markup.match(/>Member</g) ?? []).length,
    2,
    'member-rank rows (member and unknown) each carry one badge',
  );
  assert.ok(markup.includes('joined'), 'the joined line is missing');
});

test('an empty roster says so rather than rendering a hollow list', () => {
  const markup = renderToStaticMarkup(<RosterList entries={[]} profiles={new Map()} />);
  assert.ok(markup.includes('No one else is here.'));
});
