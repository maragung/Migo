/**
 * What the Friends tab's friends list is allowed to offer about the friendship itself.
 *
 * Un-friending is the friendship's affair, not the person's, so it is the one control a friend
 * row carries beside its profile door: blocking and messaging stay in the modal the door opens,
 * while the exit sits on the row because the row *is* the friendship. The rules this suite pins:
 *
 *   1. **Every friend row offers Remove friend, and stays a door.** The control never replaces
 *      the row's own bargain — tapping the row still opens the profile; only the button removes.
 *   2. **A removal in flight disables its own row's button and no one else's.** One wire call
 *      ({@link SocialDomain.removeFriend}) per row, one busy mark per call.
 *   3. **An empty list is stated, not hidden** — the honest empty state points at the
 *      suggestions, because that is where the next friend comes from.
 *
 * `renderToStaticMarkup` runs no effects and fires no events, so the confirm the panel wraps the
 * wire call in (and the re-read that follows) is not this suite's to drive; what it can pin is
 * that the list offers exactly the controls its half of the bargain needs.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry } from '@migo/sdk';

import { FriendsSection } from '../src/components/friends-panel.js';

const PROFILES = new Map<Id, { displayName: string; username?: string; avatarUrl?: string }>([
  ['user_ada' as Id, { displayName: 'Ada Lovelace', username: 'ada' }],
  ['user_grace' as Id, { displayName: 'Grace Hopper', username: 'grace' }],
]);

const FRIENDS: RelationshipEntry[] = [
  { userId: 'user_ada' as Id, kind: RelationshipKind.Friend },
  { userId: 'user_grace' as Id, kind: RelationshipKind.Friend },
];

test('every friend row offers its own exit and stays a profile door', () => {
  const markup = renderToStaticMarkup(
    <FriendsSection
      entries={FRIENDS}
      profiles={PROFILES}
      onSelect={() => {}}
      onMessage={() => {}}
      onRemove={() => {}}
    />,
  );

  assert.ok(markup.includes('Ada Lovelace'), 'a friend’s name is missing');
  assert.ok(markup.includes('@grace'), 'a friend’s username is missing');
  // One exit per row, named for what it ends — the friendship, not the person.
  assert.equal(
    (markup.match(/>Remove friend</g) ?? []).length,
    2,
    'each friend row must offer exactly one Remove friend',
  );
  // The control never replaces the row's own bargain: tapping the row still opens the profile.
  // (The apostrophe in the label is HTML-escaped in static markup, so the match pins the stable
  // prefix instead.)
  assert.equal(
    (markup.match(/aria-label="View /g) ?? []).length,
    2,
    'each friend row must still open exactly one profile',
  );
});

test('a removal in flight disables its own row and no one else’s', () => {
  const markup = renderToStaticMarkup(
    <FriendsSection
      entries={FRIENDS}
      profiles={PROFILES}
      busy={new Set<Id>(['user_ada' as Id])}
      onSelect={() => {}}
      onMessage={() => {}}
      onRemove={() => {}}
    />,
  );

  const ada = markup.slice(0, markup.indexOf('Grace'));
  const grace = markup.slice(markup.indexOf('Grace'));
  assert.ok(ada.includes('disabled'), 'a busy row must disable its Remove friend');
  assert.ok(!grace.includes('disabled'), 'an idle row’s Remove friend must stay enabled');
});

test('an empty friends list says so rather than vanishing', () => {
  const markup = renderToStaticMarkup(
    <FriendsSection
      entries={[]}
      profiles={new Map()}
      onSelect={() => {}}
      onMessage={() => {}}
      onRemove={() => {}}
    />,
  );

  assert.ok(
    markup.includes('No friends yet. Add someone from the suggestions below.'),
    'the honest empty state is missing',
  );
  assert.ok(!markup.includes('person-row'), 'an empty list must not render rows');
});
