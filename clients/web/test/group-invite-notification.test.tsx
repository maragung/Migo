/**
 * What a group invitation is allowed to look like in the bell.
 *
 * The wire writes one GroupInvite notification per freshly-seated invited account ({@link
 * NotificationKind.GroupInvite}, the enum's number 15 in the row's kind string), the inviter as
 * the actor, and the conversation named in the row. The rules this suite pins:
 *
 *   1. **The kind reads as the sentence a person was sent** — "Group invitation", not the enum's
 *      own "Group invite", which the generic camel-case rule would have produced.
 *   2. **The row can go where it points.** A group invitation is the one notification kind that
 *      names a conversation this client can open, so its row offers an Open conversation door —
 *      but only when the row carries the conversation *and* the host supplied a place to go.
 *   3. **Every other row stays inert.** A friend request never grows the door, and an invitation
 *      with no conversation to name has nowhere to go; the wire carries no destination for them.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { Id, InboxItem } from '@migo/sdk';

import { NotificationRow } from '../src/components/notifications-panel.js';

const NOOP = () => {};

/** One group invitation, as the inbox read would carry it: kind 15, an inviter, a conversation. */
function invite(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 'note_group' as Id,
    // The wire carries the kind as the NotificationKind enum's number in a string.
    kind: '15',
    at: Date.parse('2026-09-14T12:00:00Z'),
    actorId: 'acct_ada' as Id,
    conversationId: 'conv_plans' as Id,
    title: 'Weekend Plans',
    ...overrides,
  };
}

test('a group invitation reads as one, and offers the door to the conversation it names', () => {
  const markup = renderToStaticMarkup(
    <NotificationRow item={invite()} actorName="Ada Lovelace" onOpenConversation={NOOP} />,
  );

  // The sentence, not the enum's arithmetic or its own camel-case name.
  assert.ok(
    markup.includes('Ada Lovelace — Group invitation'),
    'the row must name the kind as the invitation it is',
  );
  // The conversation the invitation is about, named in the row.
  assert.ok(markup.includes('Weekend Plans'), 'the row must name the conversation');
  assert.ok(
    markup.includes('>Open conversation</button>'),
    'a group invitation naming its conversation must offer to open it',
  );
});

test('the door appears only where the row can actually go', () => {
  // No host door: the row still says what happened, but offers no navigation of its own.
  const unhosted = renderToStaticMarkup(
    <NotificationRow item={invite()} actorName="Ada Lovelace" />,
  );
  assert.ok(unhosted.includes('Group invitation'), 'the row still says what happened');
  assert.ok(
    !unhosted.includes('>Open conversation</button>'),
    'a row with nowhere to go must offer no door',
  );

  // An invitation the wire left without a conversation names no destination.
  const bare = renderToStaticMarkup(
    <NotificationRow
      item={invite({ conversationId: undefined })}
      actorName="Ada Lovelace"
      onOpenConversation={NOOP}
    />,
  );
  assert.ok(
    !bare.includes('>Open conversation</button>'),
    'an invitation naming no conversation must offer no door',
  );

  // A friend request carries a different bargain: its answer is Accept and Decline, not a door.
  const request = renderToStaticMarkup(
    <NotificationRow
      item={invite({ kind: '4', conversationId: undefined })}
      actorName="Ada Lovelace"
      onOpenConversation={NOOP}
    />,
  );
  assert.ok(request.includes('Friend request'), 'the generic kind rule must keep its sentence');
  assert.ok(
    !request.includes('>Open conversation</button>'),
    'a friend request row must not grow a conversation door',
  );
});
