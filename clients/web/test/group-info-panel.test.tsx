/**
 * What a group's roster is allowed to say about who may do what.
 *
 * A group's whole governance is two facts — who the founders are, and which controls each side of
 * that line may reach — so the gates are pinned as pure functions and the roster row's rendering is
 * pinned as markup:
 *
 *   1. **Roles are Founder or Member, nothing else.** An unknown value from a newer node renders as
 *      Member, never a guess at a rank.
 *   2. **The founder controls need a founder who is not the target and not themselves.** The two
 *      builders are beyond each other's reach, and nobody mutes or kicks their own row.
 *   3. **The vote is every member's own, but never against a founder and never against yourself.**
 *   4. **A running mute reads as a line on the row, and a departed member keeps their row without
 *      controls.** The roster is history as much as membership: a leave tombstones, it does not
 *      erase.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ConversationRole } from '@migo/sdk';
import type { ConversationRosterEntry, Id } from '@migo/sdk';

import {
  GroupRosterRow,
  canFounderAct,
  canVoteKickGroup,
  groupRoleLabel,
} from '../src/components/group-info-panel.js';

const JOINED = Date.parse('2026-08-26T12:00:00Z');
const NOW = Date.parse('2026-09-01T12:00:00Z');

function entry(overrides: Partial<ConversationRosterEntry> = {}): ConversationRosterEntry {
  return { accountId: 'user_ada' as Id, role: ConversationRole.Member, joinedAt: JOINED, ...overrides };
}

test('group roles are Founder and Member, and unknown is Member', () => {
  assert.equal(groupRoleLabel(ConversationRole.Founder), 'Founder');
  assert.equal(groupRoleLabel(ConversationRole.Member), 'Member');
  // Unknown = 0 and any value a newer node sent: Member, never a guess at a rank.
  assert.equal(groupRoleLabel(ConversationRole.Unknown), 'Member');
  assert.equal(groupRoleLabel(99), 'Member');
});

test('the founder controls need a founder who is neither the target nor themselves', () => {
  const founder = ConversationRole.Founder;
  const member = ConversationRole.Member;
  // A founder acts on a plain member.
  assert.equal(canFounderAct(founder, member, false), true);
  // A plain member reaches nothing, whoever the target is.
  assert.equal(canFounderAct(member, member, false), false);
  assert.equal(canFounderAct(member, founder, false), false);
  // The two builders are beyond each other's reach.
  assert.equal(canFounderAct(founder, founder, false), false);
  // Nobody acts on their own row.
  assert.equal(canFounderAct(founder, member, true), false);
});

test('the vote is every member\'s own, but never against a founder or themselves', () => {
  assert.equal(canVoteKickGroup(ConversationRole.Member, false), true);
  // Never against yourself.
  assert.equal(canVoteKickGroup(ConversationRole.Member, true), false);
  // Never against a founder — a show of hands cannot unseat a builder.
  assert.equal(canVoteKickGroup(ConversationRole.Founder, false), false);
  assert.equal(canVoteKickGroup(ConversationRole.Unknown, false), true);
});

test('a roster row shows the role badge and a running mute, and hides controls from plain members', () => {
  const markup = renderToStaticMarkup(
    <GroupRosterRow
      entry={entry({ mutedUntil: NOW + 60 * 60 * 1000 })}
      name="Ada Lovelace"
      now={NOW}
      canVote
      canFound
      onVoteKick={() => {}}
      onMute={() => {}}
      onUnmute={() => {}}
      onKick={() => {}}
    />,
  );
  assert.match(markup, /role-badge role-founder|role-badge role-member/);
  assert.match(markup, /Muted until/);
  // A running mute offers its undo instead of the terms that would stack a second one on top.
  assert.match(markup, /Unmute/);
  assert.doesNotMatch(markup, /Mute 1 hour/);
  assert.match(markup, /Vote kick/);
  assert.match(markup, /Kick/);
});

test('an unmuted member with founder standing sees the mute terms', () => {
  const markup = renderToStaticMarkup(
    <GroupRosterRow
      entry={entry()}
      name="Ada Lovelace"
      now={NOW}
      canFound
      onMute={() => {}}
      onKick={() => {}}
    />,
  );
  assert.match(markup, /Mute 1 hour/);
  assert.match(markup, /Mute 1 day/);
  assert.match(markup, /Mute 7 days/);
  assert.doesNotMatch(markup, /Unmute/);
});

test('a departed member keeps their row but reaches for no controls', () => {
  const markup = renderToStaticMarkup(
    <GroupRosterRow
      entry={entry({ leftAt: NOW })}
      name="Ada Lovelace"
      now={NOW}
      canVote
      canFound
      onVoteKick={() => {}}
      onMute={() => {}}
      onKick={() => {}}
    />,
  );
  assert.match(markup, /departed/);
  assert.doesNotMatch(markup, /Vote kick/);
  assert.doesNotMatch(markup, /Mute/);
  assert.doesNotMatch(markup, /Kick/);
});

test('an expired mute reads as no mute at all', () => {
  const markup = renderToStaticMarkup(
    <GroupRosterRow
      entry={entry({ mutedUntil: NOW - 1000 })}
      name="Ada Lovelace"
      now={NOW}
      canFound
      onMute={() => {}}
      onKick={() => {}}
    />,
  );
  assert.doesNotMatch(markup, /Muted until/);
  assert.doesNotMatch(markup, /Unmute/);
  assert.match(markup, /Mute 1 hour/);
});
