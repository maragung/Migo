/**
 * What the dev-mode wire counter (§171) is allowed to say about a session's bytes.
 *
 * The counter itself is the SDK transport's fact, pinned in the SDK's own suite (every byte the
 * socket carried, both directions, surviving reconnects). What this file pins is the half the
 * web client owns: the Diagnostik line renders both directions as byte sizes a person can read,
 * and a session that does not exist renders as an absence — not as a confident "0 bytes" that
 * was never measured.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { WireBytesView } from '../src/components/settings-panel.js';

test('the wire counter renders both directions as readable byte sizes', () => {
  const markup = renderToStaticMarkup(<WireBytesView bytes={{ sent: 3_072, received: 5_120 }} />);

  // Both directions, named and formatted: the developer watching a feature's cost needs to see
  // which direction grew, not just a total.
  assert.ok(
    markup.includes('Sent 3.0 KB'),
    `the sent reading is missing or unformatted: ${markup}`,
  );
  assert.ok(
    markup.includes('Received 5.0 KB'),
    `the received reading is missing or unformatted: ${markup}`,
  );
});

test('the wire counter keeps byte granularity at small sizes', () => {
  const markup = renderToStaticMarkup(<WireBytesView bytes={{ sent: 96, received: 0 }} />);

  // A reconnect costs a few hundred bytes (§56); a counter that rendered those as "0.1 KB"
  // would hide exactly the numbers the budget talks about.
  assert.ok(
    markup.includes('Sent 96 bytes'),
    `the small reading lost its byte granularity: ${markup}`,
  );
  assert.ok(
    markup.includes('Received 0 bytes'),
    `a zero direction must still be stated: ${markup}`,
  );
});

test('no session renders as an absence, never as measured zeroes', () => {
  const markup = renderToStaticMarkup(<WireBytesView bytes={null} />);

  assert.ok(!markup.includes('Sent'), 'a missing session was reported as a measured reading');
  assert.ok(
    markup.includes('No session'),
    'the absent-session line lost its plain statement of the fact',
  );
});
