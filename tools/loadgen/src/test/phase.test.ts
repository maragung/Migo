/**
 * The stall guard's contract in two parts: the tracker remembers the phase a run last entered, and
 * the message turns that phase into a sentence a reader can act on. Both are asserted because the
 * message is the entire payload of exit code 5 — a stalled run writes no report, so the sentence on
 * stderr is all anybody gets, and "loadgen exited 5" on its own would send the next reader looking
 * for a crash that never happened.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { PhaseTracker, stallMessage } from '../phase.js';

test('the tracker reports the phase the run last entered', () => {
  const tracker = new PhaseTracker();
  assert.equal(tracker.phase, 'building');
  assert.equal(tracker.finished, false);
  tracker.set('connecting');
  assert.equal(tracker.phase, 'connecting');
  tracker.set('done');
  assert.equal(tracker.finished, true);
});

test('the stall message names the phase, the await, and the drained loop', () => {
  const message = stallMessage('connecting');
  assert.match(message, /never finished/);
  assert.match(message, /opening sessions/);
  assert.match(message, /event loop is now empty/);
  assert.match(message, /must not be read as a pass/);
  // Every phase gets its own sentence: a stall in the settle phase is a different bug from a stall
  // in the connect phase, and the message is the only thing that tells them apart.
  const messages = new Set(
    (
      ['building', 'connecting', 'preparing', 'steady-state', 'settling', 'disconnecting'] as const
    ).map((phase) => stallMessage(phase)),
  );
  assert.equal(messages.size, 6);
});
