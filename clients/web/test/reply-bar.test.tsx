/**
 * What the composer is allowed to show and do about a pending reply.
 *
 * The reply bar is the only surface that says "this send will be a reply": without it a user who
 * clicked Reply an hour ago sends a threading hint they cannot see. Two rules are pinned here:
 *
 *   1. **The bar names the target and quotes the start of their message, and can be dismissed.**
 *      A bar that showed only "Replying…" would leave the user replying blind; an X that does not
 *      clear the target would thread the send to a message the user believed they had unselected.
 *   2. **No bar without a target.** A permanently mounted bar (rendered but hidden with CSS) would
 *      be announced by assistive tech that reads the DOM, not the pixels — so the test asserts the
 *      bar is genuinely absent from the markup.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { MessageComposer } from '../src/components/message-composer.js';

function render(preview: { senderName: string; snippet: string } | null): string {
  return renderToStaticMarkup(
    <MessageComposer
      onSend={() => Promise.resolve()}
      onAttach={() => Promise.resolve()}
      onTyping={() => {}}
      replyPreview={preview}
      onCancelReply={() => {}}
    />,
  );
}

test('a pending reply shows a bar naming the target and quoting their message', () => {
  const markup = render({ senderName: 'Ada', snippet: 'see you at the observatory' });

  assert.ok(markup.includes('Replying to'), 'the reply bar lost its lead-in');
  assert.ok(markup.includes('Ada'), 'the reply bar does not name who is replied to');
  assert.ok(
    markup.includes('see you at the observatory'),
    'the reply bar does not quote the target message',
  );
  assert.ok(markup.includes('aria-label="Cancel reply"'), 'the reply bar lost its dismiss control');
});

test('without a reply target there is no bar at all', () => {
  assert.ok(!render(null).includes('Replying to'), 'a reply bar rendered with no target');
});
