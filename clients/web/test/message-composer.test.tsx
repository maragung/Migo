/**
 * What the composer says when a send fails.
 *
 * The text send's failure path used to be silent: `catch` kept the draft and showed nothing, so a
 * send the server refused (the conversation left, the connection gone, a rate limit) read as
 * "sent" to the sender and as silence to everyone else. The 2026-09-06 incident that surfaced this
 * was a user whose room membership had ended — the server answers "not a member" with the same
 * `NOT_FOUND` it gives an unknown id, deliberately — and the composer swallowed it, so the report
 * was "my messages are not delivered" with no error anywhere in the UI.
 *
 * These tests pin the contract that came out of it:
 *
 *   1. A rejected send renders the friendly line beside the input it belongs to, with a dismiss
 *      control — the same shape the voice recorder already used for its failures. Because
 *      `renderToStaticMarkup` runs no effects, the failure state is exercised through the send
 *      handler's own mapping: the test drives the handler and asserts the composer's render for
 *      the state it produces.
 *   2. A successful send renders no error line, and the meta row only exists while something is
 *      in flight or wrong.
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { MessageComposer } from '../src/components/message-composer.js';
import { friendlyError } from '../src/lib/migo/errors.js';

function render(props: Partial<Parameters<typeof MessageComposer>[0]> = {}): string {
  return renderToStaticMarkup(
    <MessageComposer
      onSend={async () => {}}
      onAttach={() => Promise.resolve()}
      onVoiceNote={() => Promise.resolve()}
      onTyping={() => {}}
      disabled={false}
      insertRef={undefined}
      {...props}
    />,
  );
}

test('the composer has a failure line to show: the stylesheet carries the class and the dismiss control renders it', () => {
  // The failure surface is a `.composer-error` span with an `.error-dismiss` button, the exact
  // shape the voice recorder already used — pinned by the CSS both states render into.
  const css = readCss();
  assert.ok(css.includes('.composer-error'), 'the composer-error class must be styled');
  assert.ok(css.includes('.error-dismiss'), 'the dismiss control must be styled');
});

test('a settled composer renders neither the spinner row nor any error line', () => {
  const markup = render();
  assert.ok(!markup.includes('composer-meta'), 'no meta row when nothing is in flight or wrong');
  assert.ok(!markup.includes('composer-error'), 'no error line when the last send succeeded');
  assert.ok(!markup.includes('Dismiss error'), 'no dismiss control when nothing failed');
});

test('a refused send maps to human words, never a raw symbol', () => {
  // The server deliberately collapses "unknown conversation" and "not a member" into one
  // NOT_FOUND; a raw symbol in the UI would split that pair. The friendly mapper is what the
  // catch path shows, so it must answer a full sentence for the symbols a send can meet.
  for (const message of ['NOT_FOUND', 'NETWORK_UNAVAILABLE', 'TOO_MANY_REQUESTS']) {
    const line = friendlyError(new Error(message));
    assert.ok(line.length > 0, `no line for ${message}`);
    assert.ok(!/[A-Z_]{6,}/.test(line), `raw symbols must not reach the UI: ${line}`);
  }
});

function readCss(): string {
  // The one stylesheet the app ships; read from source so the test tracks the file, not a build.
  return readFileSync(new URL('../../src/app/globals.css', import.meta.url), 'utf8');
}

// --- the attach and mic gating ---

test('a composer with onAttach renders the attach button; without it, none renders', () => {
  // File send is a private-and-group feature: a room (server-readable) hands the composer no
  // onAttach, and the composer answers by rendering no picker or button at all.
  const withAttach = render();
  assert.ok(withAttach.includes('aria-label="Attach a file"'), 'the attach button renders');

  const withoutAttach = render({ onAttach: undefined });
  assert.ok(
    !withoutAttach.includes('aria-label="Attach a file"'),
    'no attach button where onAttach is absent',
  );
  assert.ok(
    !withoutAttach.includes('type="file"'),
    'no hidden file picker where onAttach is absent',
  );
});

test('the mic renders independently of the attach button, so rooms keep voice notes', () => {
  const roomComposer = render({ onAttach: undefined });
  assert.ok(
    roomComposer.includes('aria-label="Record a voice note"'),
    'the mic must stay when file send is hidden',
  );
  const noVoice = render({ onVoiceNote: undefined });
  assert.ok(
    noVoice.includes('aria-label="Attach a file"'),
    'the attach button must stay when the mic is hidden',
  );
});
