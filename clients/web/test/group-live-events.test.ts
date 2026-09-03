/**
 * The pure projections that keep the conversation list in step with a group's live streams.
 *
 * A group's member events and state deltas arrive as wire data; the list holds summaries. The two
 * must meet somewhere, and {@link applyMemberEvent} / {@link applyStateEvent} are that place — pure,
 * so a test can pin exactly what each event costs the summary:
 *
 *   1. **A join adds the account; a leave, kick, or ban removes it.** A join for someone already
 *      listed and a departure for someone already gone change nothing.
 *   2. **A summary without a member list is left alone.** The wire's roster knowledge is optional;
 *      inventing it from an event would fabricate membership.
 *   3. **A state delta writes only the fields it carries.** A rename writes the title; an empty
 *      delta writes nothing.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { ConversationKind, EncryptionMode, MemberChange } from '@migo/sdk';
import type {
  ConversationMemberEvent,
  ConversationStateEvent,
  ConversationSummary,
  Id,
} from '@migo/sdk';

import { applyMemberEvent, applyStateEvent } from '../src/lib/migo/conversations-provider.js';

const ALICE = 'user_alice' as Id;
const BOB = 'user_bob' as Id;
const CAROL = 'user_carol' as Id;

function summary(overrides: Partial<ConversationSummary> = {}): ConversationSummary {
  return {
    conversationId: 'conv_1' as Id,
    kind: ConversationKind.Group,
    encryption: EncryptionMode.EndToEnd,
    lastSeq: 10,
    readSeq: 10,
    members: [ALICE, BOB],
    ...overrides,
  };
}

function memberEvent(change: MemberChange, userId: Id): ConversationMemberEvent {
  return { conversationId: 'conv_1' as Id, userId, change, memberCount: 2 };
}

test('a join adds the account to the member list', () => {
  const next = applyMemberEvent(summary(), memberEvent(MemberChange.Joined, CAROL));
  assert.deepEqual(next.members, [ALICE, BOB, CAROL]);
});

test('a join for someone already seated changes nothing', () => {
  const held = summary();
  assert.equal(applyMemberEvent(held, memberEvent(MemberChange.Joined, BOB)), held);
});

test('a leave, a kick, and a ban each remove the account', () => {
  for (const change of [MemberChange.Left, MemberChange.Kicked, MemberChange.Banned]) {
    const next = applyMemberEvent(summary(), memberEvent(change, BOB));
    assert.deepEqual(next.members, [ALICE], `${MemberChange[change]} removes the member`);
  }
});

test('a departure for someone already gone changes nothing', () => {
  const held = summary({ members: [ALICE] });
  assert.equal(applyMemberEvent(held, memberEvent(MemberChange.Kicked, BOB)), held);
});

test('a summary without a member list is left alone', () => {
  const held = summary();
  delete held.members;
  assert.equal(applyMemberEvent(held, memberEvent(MemberChange.Joined, CAROL)), held);
});

test('the rest of the summary rides along untouched', () => {
  const next = applyMemberEvent(
    summary({ title: 'Weekend plans' }),
    memberEvent(MemberChange.Kicked, BOB),
  );
  assert.equal(next.title, 'Weekend plans');
  assert.equal(next.conversationId, 'conv_1');
  assert.equal(next.kind, ConversationKind.Group);
});

test('a rename writes the title the delta carries', () => {
  const delta: ConversationStateEvent = {
    conversationId: 'conv_1' as Id,
    title: 'Weekend plans, revised',
  };
  const next = applyStateEvent(summary({ title: 'Weekend plans' }), delta);
  assert.equal(next.title, 'Weekend plans, revised');
});

test('a delta without a title writes nothing', () => {
  const held = summary({ title: 'Weekend plans' });
  const delta: ConversationStateEvent = { conversationId: 'conv_1' as Id };
  assert.equal(applyStateEvent(held, delta), held);
});

test('a rename to the held title changes nothing', () => {
  const held = summary({ title: 'Weekend plans' });
  const delta: ConversationStateEvent = { conversationId: 'conv_1' as Id, title: 'Weekend plans' };
  assert.equal(applyStateEvent(held, delta), held);
});
