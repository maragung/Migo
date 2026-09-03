/**
 * What a personal mute is allowed to hide, and what the Muted section is allowed to say.
 *
 * A mute is the gentler cousin of a block: it hides a person's *room* chatter for the muter and
 * nothing else — delivery, friendship, and direct messages are all untouched. Two rules carry the
 * weight:
 *
 *   1. **The filter hides muted senders and no one else — and is a no-op when nothing is muted.**
 *      {@link muteFilter} returns the very same array reference for an empty set, so a caller's memo
 *      does not churn; with a set, only the muted senders fall away. Where it runs (room transcripts,
 *      never direct threads) is the caller's decision, pinned at the call site, not here.
 *   2. **The Muted section states an empty list rather than vanishing, and offers a one-tap undo.**
 *      Unlike a block, a mute has an unmute on the wire, so each row carries it; an empty section
 *      says you have muted no one rather than reading as a broken feature.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry } from '@migo/sdk';

import { muteFilter } from '../src/lib/migo/muted-provider.js';
import { MutedSection } from '../src/components/friends-panel.js';

interface Msg {
  messageId: string;
  senderId: Id;
}

function msg(messageId: string, senderId: string): Msg {
  return { messageId, senderId: senderId as Id };
}

test('an empty mute set returns the same array, so a caller’s memo does not churn', () => {
  const messages = [msg('m1', 'user_ada'), msg('m2', 'user_bob')];
  const out = muteFilter(messages, new Set<Id>());
  assert.equal(out, messages, 'an empty set must return the input reference unchanged');
});

test('a mute set hides exactly the muted senders and keeps the rest', () => {
  const messages = [
    msg('m1', 'user_ada'),
    msg('m2', 'user_bob'),
    msg('m3', 'user_ada'),
    msg('m4', 'user_cleo'),
  ];
  const out = muteFilter(messages, new Set<Id>(['user_ada' as Id]));
  assert.deepEqual(
    out.map((message) => message.messageId),
    ['m2', 'm4'],
    'every message from a muted sender must fall away, and only those',
  );
});

const MUTED: RelationshipEntry[] = [
  { userId: 'user_ada' as Id, kind: RelationshipKind.Mute },
  { userId: 'user_bob' as Id, kind: RelationshipKind.Mute },
];

const PROFILES = new Map<Id, { displayName: string; username?: string }>([
  ['user_ada' as Id, { displayName: 'Ada Lovelace', username: 'ada' }],
  ['user_bob' as Id, { displayName: 'Bob Kahn', username: 'bob' }],
]);

test('the muted section lists each muted account with a one-tap unmute', () => {
  const markup = renderToStaticMarkup(
    <MutedSection entries={MUTED} profiles={PROFILES} onSelect={() => {}} onUnmute={() => {}} />,
  );

  assert.ok(markup.includes('Muted'), 'the section lost its heading');
  assert.ok(markup.includes('Ada Lovelace'), 'a muted account’s name is missing');
  assert.ok(markup.includes('@bob'), 'a muted account’s username is missing');
  // The note names what a mute does and does not do.
  assert.ok(markup.includes('room messages hidden'), 'the row lost its mute note');
  // One unmute per row, and each row is still a door to the profile.
  assert.equal(
    (markup.match(/>Unmute</g) ?? []).length,
    2,
    'each muted row must offer exactly one unmute',
  );
  assert.equal(
    (markup.match(/aria-label="View /g) ?? []).length,
    2,
    'each muted row must open exactly one profile',
  );
});

test('an empty mute list says so rather than vanishing', () => {
  const markup = renderToStaticMarkup(
    <MutedSection entries={[]} profiles={new Map()} onSelect={() => {}} onUnmute={() => {}} />,
  );
  assert.ok(markup.includes('No muted accounts.'), 'the honest empty state is missing');
  assert.ok(!markup.includes('person-row'), 'an empty section must not render rows');
});
