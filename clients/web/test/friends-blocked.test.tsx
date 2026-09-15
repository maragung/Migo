/**
 * What the Friends tab's Blocked section is allowed to say about the block list.
 *
 * The block list comes from the whole-graph read ({@link SocialDomain.listAllRelationships}),
 * filtered to Block-kind edges — the only place the wire ever names them. Three rules carry
 * correctness weight:
 *
 *   1. **An empty block list is stated, not hidden.** A vanished section would read as "blocking
 *      is broken"; an honest empty state reads as "you block no one".
 *   2. **Every blocked row is a door to the person's profile.** The block state and any further
 *      action live in the modal, not on the row, so the list stays clean — the row offers its
 *      one action and no more.
 *   3. **The row's one action is its own undo.** The block is the section's whole subject, so
 *      lifting it ({@link SocialDomain.unblockUser}) belongs beside the door, disabled only
 *      while its own wire call flies. Unblocking restores nothing the block tore down, so it
 *      asks for no confirm.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry } from '@migo/sdk';

import { BlockedSection } from '../src/components/friends-panel.js';

const PROFILES = new Map<Id, { displayName: string; username?: string }>([
  ['user_ada' as Id, { displayName: 'Ada Lovelace', username: 'ada' }],
  ['user_grace' as Id, { displayName: 'Grace Hopper', username: 'grace' }],
]);

const BLOCKED: RelationshipEntry[] = [
  { userId: 'user_ada' as Id, kind: RelationshipKind.Block },
  { userId: 'user_grace' as Id, kind: RelationshipKind.Block },
];

test('the blocked section lists every blocked account as a profile door', () => {
  const markup = renderToStaticMarkup(
    <BlockedSection
      entries={BLOCKED}
      profiles={PROFILES}
      onSelect={() => {}}
      onUnblock={() => {}}
    />,
  );

  assert.ok(markup.includes('Blocked'), 'the section lost its heading');
  assert.ok(markup.includes('Ada Lovelace'), 'a blocked account’s name is missing');
  assert.ok(markup.includes('@grace'), 'a blocked account’s username is missing');
  assert.ok(markup.includes('blocked'), 'the row lost its blocked note');
  // One door per row, named for the person it opens. (The apostrophe in the label is
  // HTML-escaped in static markup, so the match pins the stable prefix instead.)
  assert.equal(
    (markup.match(/aria-label="View /g) ?? []).length,
    2,
    'each blocked row must open exactly one profile',
  );
});

test('each blocked row carries its own unblock, disabled only while it flies', () => {
  const markup = renderToStaticMarkup(
    <BlockedSection
      entries={BLOCKED}
      profiles={PROFILES}
      busy={new Set<Id>(['user_grace' as Id])}
      onSelect={() => {}}
      onUnblock={() => {}}
    />,
  );

  // One unblock per row, so the block is lifted right where the list states it.
  assert.equal(
    (markup.match(/>Unblock</g) ?? []).length,
    2,
    'each blocked row must offer exactly one unblock',
  );
  // A row whose unblock is in flight disables its own button and no one else's.
  const ada = markup.slice(0, markup.indexOf('Grace'));
  const grace = markup.slice(markup.indexOf('Grace'));
  assert.ok(!ada.includes('disabled'), 'an idle row’s unblock must stay enabled');
  assert.ok(grace.includes('disabled'), 'a busy row must disable its unblock');
});

test('an empty block list says so rather than vanishing', () => {
  const markup = renderToStaticMarkup(
    <BlockedSection entries={[]} profiles={new Map()} onSelect={() => {}} onUnblock={() => {}} />,
  );

  assert.ok(markup.includes('No blocked accounts.'), 'the honest empty state is missing');
  assert.ok(!markup.includes('person-row'), 'an empty section must not render rows');
});
