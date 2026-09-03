/**
 * What the presence controls are allowed to say and publish.
 *
 * Both controls are fully controlled — the banner owns the current state and performs the
 * publish — so the tests pin the rendering contract over plain props:
 *
 *   1. **The four self-reportable states are the only options.** Online, Away, Busy, Invisible —
 *      exactly what the protocol's enum offers a person to say about themselves.
 *   2. **The status input is capped at its wire bound and seeded with the current status.** The
 *      input's maxlength is the profile field's 100 characters, and the draft it starts from is
 *      the status the parent holds.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { PresenceState } from '@migo/sdk';

import {
  PresenceSelect,
  STATUS_MAX_CHARS,
  StatusInput,
} from '../src/components/presence-picker.js';

function renderSelect(state: PresenceState): string {
  return renderToStaticMarkup(<PresenceSelect state={state} onStateChange={() => {}} />);
}

test('the dropdown offers exactly the four self-reportable presence states', () => {
  const markup = renderSelect(PresenceState.Online);

  const options = markup.match(/<option[^>]*>([^<]*)<\/option>/g) ?? [];
  assert.equal(options.length, 4, 'the dropdown must offer exactly four states');
  for (const label of ['Online', 'Away', 'Busy', 'Invisible']) {
    assert.ok(
      (markup.match(new RegExp(`>${label}<\\/option>`)) ?? []).length === 1,
      `the "${label}" state is missing`,
    );
  }
  // The current state is the selected option: Online renders with the selected mark.
  assert.ok(
    /<option value="2" selected="">Online<\/option>/.test(markup),
    'the current state must be the select’s selected option',
  );
});

test('a non-default state is the one selected', () => {
  const markup = renderSelect(PresenceState.Invisible);
  assert.ok(
    /<option value="5" selected="">Invisible<\/option>/.test(markup),
    'the dropdown must mark the chosen state, not a hard-coded one',
  );
});

test('the status input is capped at its wire bound and seeded with the current status', () => {
  const markup = renderToStaticMarkup(
    <StatusInput state={PresenceState.Busy} status="shipping the release" onChange={() => {}} />,
  );

  assert.ok(
    markup.includes(`maxLength="${STATUS_MAX_CHARS}"`),
    `the status input lost its ${STATUS_MAX_CHARS}-character cap`,
  );
  assert.ok(
    markup.includes('value="shipping the release"'),
    'the status input did not carry the current status',
  );
  assert.ok(
    markup.includes('placeholder="Set a status…"'),
    'the status input must say what it is for when it is empty',
  );
});

test('the status input carries no current-status line of its own', () => {
  // The stated-status line lived under the old single picker; the banner now states the status
  // by seeding the input itself, so an extra line would say the same thing twice.
  const markup = renderToStaticMarkup(
    <StatusInput state={PresenceState.Away} status="" onChange={() => {}} />,
  );
  assert.ok(!markup.includes('presence-current'), 'no duplicate status line belongs to the input');
});
