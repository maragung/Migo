/**
 * What the trailing-edge debounce owes the surfaces that re-read on friend events.
 *
 * A friend event says the graph moved, not how, so every surface re-reads the whole graph — and
 * the wire's coalescing means one acceptance can arrive as several events, each echoed per
 * device. The debounce is what keeps that honest rate honest: one read per burst of quiet, made
 * with the last call's arguments (the last event is the freshest word on what moved), and a
 * pending read that dies with the surface that scheduled it. The rules this file pins:
 *
 *   1. **A burst inside the window costs one invocation, with the last arguments.** A read fired
 *      mid-burst would fetch a graph the next event was about to supersede anyway.
 *   2. **Calls separated by a full window are separate questions.** The window reopens after a
 *      firing, so a later burst is never swallowed by an earlier one.
 *   3. **Cancel drops a pending call without ever firing it** — the cleanup an effect owes an
 *      unmounted surface — and is safe to call when nothing is pending.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { debounce } from '../src/lib/debounce.js';

/** Real timers, with margins wide enough that a loaded CI runner cannot blur the windows. */
const QUIET_MS = 25;
const BURST_GAP_MS = 5;
const SETTLE_MS = 60;

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test('a burst of calls inside the quiet window costs one invocation, made with the last arguments', async () => {
  const calls: string[] = [];
  const reRead = debounce((who: string) => {
    calls.push(who);
  }, QUIET_MS);

  reRead('the request leaving');
  await wait(BURST_GAP_MS);
  reRead('the friendship arriving');
  await wait(BURST_GAP_MS);
  reRead('the other device’s echo');

  // Still inside the window: nothing has fired, and the burst is not over.
  assert.deepEqual(calls, [], 'the quiet window has not closed yet');

  await wait(SETTLE_MS);
  assert.deepEqual(
    calls,
    ['the other device’s echo'],
    'one read per burst, carrying the last event’s arguments',
  );
});

test('calls separated by a full window are separate questions, not one burst', async () => {
  const calls: number[] = [];
  const tick = debounce((n: number) => {
    calls.push(n);
  }, QUIET_MS);

  tick(1);
  await wait(SETTLE_MS);
  assert.deepEqual(calls, [1], 'the first call fired on its own');

  tick(2);
  await wait(SETTLE_MS);
  assert.deepEqual(calls, [1, 2], 'the window reopened, so the later call fired too');
});

test('cancel drops the pending call without ever firing it, and tolerates nothing pending', async () => {
  const calls: number[] = [];
  const tick = debounce((n: number) => {
    calls.push(n);
  }, QUIET_MS);

  tick(1);
  tick.cancel();
  await wait(SETTLE_MS);
  assert.deepEqual(calls, [], 'a cancelled read must never fire');

  // An unmounted surface's cleanup may run when nothing was scheduled; that must be a no-op,
  // not an error, because effects tear down on every dependency change.
  tick.cancel();
  tick(2);
  await wait(SETTLE_MS);
  assert.deepEqual(calls, [2], 'the debounce still works after a cancel');
});
