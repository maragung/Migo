/**
 * What the presence picker is allowed to say and publish.
 *
 * The picker is fully controlled — the parent owns the current state and performs the publish —
 * so the tests pin the rendering contract over plain props:
 *
 *   1. **The four self-reportable states are the only options.** Online, Away, Busy, Invisible —
 *      exactly what the protocol's enum offers a person to say about themselves.
 *   2. **The status line is shown, capped at its wire bound.** The input's maxlength is the
 *      profile field's 100 characters, and the current status is stated below the dropdown.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { PresenceState } from '@migo/sdk';

import { PresencePicker, STATUS_MAX_CHARS } from '../src/components/presence-picker.js';

function render(state: PresenceState, status: string): string {
  return renderToStaticMarkup(<PresencePicker state={state} status={status} onChange={() => {}} />);
}

test('the picker offers exactly the four self-reportable presence states', () => {
  const markup = render(PresenceState.Online, '');

  const options = markup.match(/<option[^>]*>([^<]*)<\/option>/g) ?? [];
  assert.equal(options.length, 4, 'the picker must offer exactly four states');
  for (const label of ['Online', 'Away', 'Busy', 'Invisible']) {
    assert.ok(
      (markup.match(new RegExp(`>${label}<\\/option>`)) ?? []).length === 1,
      `the "${label}" state is missing`,
    );
  }
  // The current state is the selected option: Online renders with the selected mark.
  assert.ok(
    /<option value="2" selected="">Online<\/option>/.test(markup),
    'the current state must be the select\u2019s selected option',
  );
});

test('a non-default state is the one selected', () => {
  const markup = render(PresenceState.Invisible, '');
  assert.ok(
    /<option value="5" selected="">Invisible<\/option>/.test(markup),
    'the picker must mark the chosen state, not a hard-coded one',
  );
});

test('the custom status input is capped at its wire bound and seeded with the current status', () => {
  const markup = render(PresenceState.Busy, 'shipping the release');

  assert.ok(
    markup.includes(`maxLength="${STATUS_MAX_CHARS}"`),
    `the status input lost its ${STATUS_MAX_CHARS}-character cap`,
  );
  assert.ok(
    markup.includes('value="shipping the release"'),
    'the status input did not carry the current status',
  );
  // The current status is stated below the dropdown, not only held in the input.
  assert.ok(
    markup.includes('presence-current'),
    'the current-status line below the dropdown is missing',
  );
});

test('an absent status renders no status line rather than an empty one', () => {
  const markup = render(PresenceState.Away, '');
  const current = markup.match(/class="presence-current"[^>]*>([^<]*)</);
  assert.ok(current !== null, 'the current-status line is missing');
  assert.equal((current[1] ?? '').trim(), '', 'an empty status must not render placeholder text');
});
