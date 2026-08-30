/**
 * What a message row is allowed to offer as edit and reaction controls.
 *
 * The hover actions grew two new affordances — Edit on our own text messages, and a quick
 * reaction bar on every message — and both are permission-gated in ways a "helpful" refactor
 * would silently break:
 *
 *   1. **Edit is ours-only, text-only, and caller-gated.** A peer's message is never editable, a
 *      media message is never editable (its content is an object reference, not a draft), and a
 *      context that cannot commit an edit (`onEdit` absent) must not offer one at all.
 *   2. **The reaction bar is per-message and labelled.** One button per quick emoji, named for
 *      the emoji it sends, and absent entirely when the caller cannot commit a reaction.
 *   3. **A message once edited wears its edited mark.** The wire's `editedAt` stamp must
 *      surface, or an edited message would masquerade as the original wording forever.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { ContentType } from '@migo/sdk';
import type { Id, UserProfile } from '@migo/sdk';

import { MessageList } from '../src/components/message-list.js';
import type { ThreadMessage } from '../src/lib/migo/use-chat.js';

const CREATED = Date.parse('2026-08-26T12:00:00Z');

const PROFILES = new Map<Id, UserProfile>([
  ['ada' as Id, { userId: 'ada' as Id, publicId: 'MGO-ADA', username: 'ada', displayName: 'Ada' }],
]);

let seq = 0;
function msg(fields: { senderId: Id; text: string; editedAt?: number }): ThreadMessage {
  seq += 1;
  return {
    messageId: `msg_${seq}` as Id,
    conversationId: 'conv_1' as Id,
    seq,
    senderId: fields.senderId,
    senderDevice: 'dev_1' as Id,
    content: { type: ContentType.Text, text: fields.text },
    createdAt: CREATED,
    ...(fields.editedAt !== undefined ? { editedAt: fields.editedAt } : {}),
  };
}

function render(
  messages: ThreadMessage[],
  props: { onEdit?: boolean; onReact?: boolean } = {},
): string {
  return renderToStaticMarkup(
    <MessageList
      messages={messages}
      selfId={'me' as Id}
      showSenders={false}
      profiles={PROFILES}
      readUpTo={0}
      onReply={() => {}}
      onDelete={() => {}}
      {...(props.onEdit !== false ? { onEdit: () => {} } : {})}
      {...(props.onReact !== false ? { onReact: () => {} } : {})}
      deleting={false}
      hasEarlier={false}
      loadingEarlier={false}
      onLoadEarlier={() => {}}
    />,
  );
}

test('our own text messages offer an Edit control alongside Reply and Delete', () => {
  const markup = render([msg({ senderId: 'me' as Id, text: 'my words' })]);

  assert.ok(
    markup.includes('aria-label="Edit message"'),
    'our own text message lost its edit control',
  );
  assert.ok(markup.includes('aria-label="Reply to You"'), 'the reply control was displaced');
  assert.ok(markup.includes('aria-label="Delete message"'), 'the delete control was displaced');
});

test("a peer's message is never editable", () => {
  const markup = render([msg({ senderId: 'ada' as Id, text: 'their words' })]);

  assert.ok(
    !markup.includes('aria-label="Edit message"'),
    'a peer\u2019s message was offered an edit control',
  );
  // The peer's message still carries the actions that are theirs to receive.
  assert.ok(markup.includes('aria-label="Reply to Ada"'), 'the reply control was displaced');
});

test('a context that cannot commit edits offers none at all', () => {
  const markup = render([msg({ senderId: 'me' as Id, text: 'my words' })], { onEdit: false });

  assert.ok(!markup.includes('aria-label="Edit message"'), 'an uncommittable edit was offered');
});

test('every message carries the quick reaction bar, one labelled button per emoji', () => {
  const markup = render([
    msg({ senderId: 'ada' as Id, text: 'their words' }),
    msg({ senderId: 'me' as Id, text: 'my words' }),
  ]);

  for (const emoji of ['👍', '❤️', '😂']) {
    assert.equal(
      (markup.match(new RegExp(`aria-label="React with ${emoji}"`, 'g')) ?? []).length,
      2,
      `each message must offer the ${emoji} reaction exactly once`,
    );
  }
  // The bar is grouped and named for its target, so assistive tech reads one control set.
  assert.ok(
    (markup.match(/role="group" aria-label="React to /g) ?? []).length === 2,
    'each message must carry exactly one reaction group',
  );
});

test('a context that cannot commit reactions renders no reaction bar', () => {
  const markup = render([msg({ senderId: 'ada' as Id, text: 'their words' })], {
    onReact: false,
  });

  assert.ok(!markup.includes('reaction-bar'), 'an uncommittable reaction bar was rendered');
});

test('a message the wire stamped as edited wears its edited mark', () => {
  const stamped = render([
    msg({ senderId: 'me' as Id, text: 'my words, corrected', editedAt: CREATED }),
  ]);
  assert.ok(stamped.includes('edited'), 'an edited message lost its edited mark');

  const plain = render([msg({ senderId: 'me' as Id, text: 'my words' })]);
  assert.ok(!plain.includes('edited'), 'an unedited message was marked as edited');
});
