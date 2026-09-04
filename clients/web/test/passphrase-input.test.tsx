/**
 * What a passphrase field is allowed to look like on first paint.
 *
 * The mask is the point of {@link PassphraseInput}: a passphrase renders as one dot per character
 * (`type="password"`, the browser's own masking), and the eye beside it is the only way to see
 * the text. The field it replaced carried `type="passphrase"` — not a real input type, so
 * browsers rendered it as *plain text*, and every passphrase sat on screen unmasked until
 * someone noticed. That regression is what this test pins against: the masked default, and the
 * toggle that names both of its states.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { PassphraseInput } from '../src/components/passphrase-input.js';

test('a passphrase field is masked on first paint, with the eye beside it', () => {
  const markup = renderToStaticMarkup(
    <PassphraseInput
      value="correct-horse-battery-staple"
      onChange={() => undefined}
      autoComplete="current-passphrase"
      required
    />,
  );

  assert.ok(markup.includes('type="password"'), 'the field must be masked by default');
  assert.ok(
    !markup.includes('type="text"'),
    'a fresh field must not render the passphrase as text',
  );
  assert.ok(
    markup.includes('aria-label="Show passphrase"'),
    'the toggle must name what pressing it does',
  );
  assert.ok(
    markup.includes('aria-pressed="false"'),
    'the toggle must say the passphrase is currently hidden',
  );
});
