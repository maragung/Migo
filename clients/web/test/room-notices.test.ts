/**
 * The line a membership change reads out.
 *
 * {@link memberNotice} is the pure core of the open room's activity strip: every branch of the
 * {@link MemberChange} enum maps to one sentence, an absent or unknown change falls back to the
 * legacy join/leave flag, and an unresolved name becomes "Someone" rather than a blank or an id.
 * The hook around it only buffers these notices as they arrive; the words are here, so the words
 * are what a test pins — a regression would quietly turn "was banned" into "left" and no functional
 * test would notice.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { MemberChange } from '@migo/sdk';

import { memberNotice } from '../src/lib/migo/use-room-notices.js';

test('every membership change maps to its own sentence', () => {
  assert.equal(memberNotice(MemberChange.Joined, 'Ana'), 'Ana joined the room');
  assert.equal(memberNotice(MemberChange.Left, 'Ana'), 'Ana left');
  assert.equal(memberNotice(MemberChange.Disconnected, 'Ana'), 'Ana disconnected');
  assert.equal(memberNotice(MemberChange.Reconnected, 'Ana'), 'Ana came back');
  assert.equal(memberNotice(MemberChange.Kicked, 'Ana'), 'Ana was kicked');
  assert.equal(memberNotice(MemberChange.Banned, 'Ana'), 'Ana was banned');
});

test('an absent or unknown change falls back to the legacy join/leave flag', () => {
  // A legacy event carries no `change`, only the joined boolean.
  assert.equal(memberNotice(undefined, 'Ana', true), 'Ana joined the room');
  assert.equal(memberNotice(undefined, 'Ana', false), 'Ana left');
  // The Unknown sentinel is treated the same as absent — a guess at a verb would invent an event.
  assert.equal(memberNotice(MemberChange.Unknown, 'Ana', false), 'Ana left');
  assert.equal(memberNotice(MemberChange.Unknown, 'Ana', true), 'Ana joined the room');
  // The joined flag defaults to true, so a bare call still reads as a join.
  assert.equal(memberNotice(undefined, 'Ana'), 'Ana joined the room');
});

test('an unresolved name becomes "Someone", never a blank or an id', () => {
  assert.equal(memberNotice(MemberChange.Joined, ''), 'Someone joined the room');
  assert.equal(memberNotice(MemberChange.Kicked, '   '), 'Someone was kicked');
  assert.equal(memberNotice(undefined, '', false), 'Someone left');
});
