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

test('the evidence clause says how far the virtual users got', () => {
  const tracker = new PhaseTracker();
  // The empty tracker is the loudest case: nothing ever reached the gateway, so the stall is before
  // the first session rather than inside one — and a bare phase name cannot say that.
  assert.match(tracker.snapshot(), /Not one virtual user reached the gateway/);

  tracker.observeState('connecting');
  tracker.observeState('connecting');
  tracker.observeConnect(false);
  const message = stallMessage(tracker.phase, tracker.snapshot());
  assert.match(message, /0 virtual user\(s\) had finished connecting and 1 had failed/);
  assert.match(message, /connecting x2/);
  // The evidence is appended, never swapped in: the phase sentence is what makes the exit code mean
  // anything, and a reader who only gets the tally still cannot tell a stall from a slow run.
  assert.match(message, /event loop is now empty/);
});

test('the evidence clause is omitted when there is none, with no dangling separator', () => {
  assert.equal(stallMessage('preparing'), stallMessage('preparing', ''));
  assert.doesNotMatch(stallMessage('preparing'), /\s$/);
});
