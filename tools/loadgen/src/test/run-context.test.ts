/**
 * The engine every scenario runs on. Two guarantees have to hold or a run's numbers are fiction:
 * `measure` must never throw (one failed op cannot tear down a VU's loop) and must count a failure
 * by class rather than swallow it, and `paceLoop` must stop exactly at its stop condition — the
 * deadline or an interrupt — and not one iteration later. The back-off path is checked both ways:
 * that it records the error, and that a server's `retryAfterMs` actually delays the resolution.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { RemoteError, TransportError } from '@migo/sdk';

import { Logger } from '../logger.js';
import { RunContext, sleep } from '../run-context.js';
import { Metrics } from '../stats.js';

const QUIET = new Logger('quiet');
const future = (): number => performance.now() + 60_000;

test('measure records latency and a success when the operation resolves', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  await ctx.measure('op', async () => {});
  const op = metrics.operation('op');
  assert.equal(op.ok, 1);
  assert.equal(op.errors, 0);
  assert.equal(op.latency.count, 1);
});

test('measure counts a failure by class and never throws', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  await assert.doesNotReject(() =>
    ctx.measure('op', () => Promise.reject(new TransportError('gone'))),
  );
  const op = metrics.operation('op');
  assert.equal(op.ok, 0);
  assert.equal(op.errors, 1);
  assert.deepEqual(op.errorsByClass, [['transport', 1]]);
});

test('measure classifies a server refusal and, when already interrupted, backs off instantly', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  ctx.interrupt(); // makes the bounded back-off sleep return immediately
  await ctx.measure('op', () => Promise.reject(new RemoteError(429, 'RATE_LIMITED', '', 10_000)));
  assert.deepEqual(metrics.operation('op').errorsByClass, [['remote:RATE_LIMITED', 1]]);
});

test('measure honours a server back-off by delaying its resolution', async () => {
  const metrics = new Metrics();
  const ctx = new RunContext(metrics, QUIET, 0, future());
  const started = performance.now();
  await ctx.measure('op', () => Promise.reject(new RemoteError(429, 'RATE_LIMITED', '', 40)));
  const elapsed = performance.now() - started;
  assert.ok(elapsed >= 20, `expected a back-off wait, took only ${elapsed.toFixed(1)}ms`);
  assert.equal(metrics.operation('op').errors, 1);
});

test('paceLoop runs zero times when the deadline has already passed', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, performance.now() - 1000);
  let iterations = 0;
  await ctx.paceLoop(() => {
    iterations += 1;
    return Promise.resolve();
  });
  assert.equal(iterations, 0);
});

test('paceLoop stops exactly when interrupted, running no further iteration', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  let count = 0;
  await ctx.paceLoop(() => {
    count += 1;
    if (count === 3) ctx.interrupt();
    return Promise.resolve();
  });
  assert.equal(count, 3);
});

test('paceLoop with a target rate still stops promptly on interrupt', async () => {
  const ctx = new RunContext(new Metrics(), QUIET, 100, future());
  let count = 0;
  await ctx.paceLoop(() => {
    count += 1;
    if (count === 2) ctx.interrupt();
    return Promise.resolve();
  });
  assert.equal(count, 2);
});

test('interrupt flips the flag and forces deadlineReached regardless of the clock', () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, future());
  assert.equal(ctx.interrupted, false);
  assert.equal(ctx.deadlineReached(), false);
  ctx.interrupt();
  assert.equal(ctx.interrupted, true);
  assert.equal(ctx.deadlineReached(), true);
});

test('setDeadline moves the deadline, and msRemaining is clamped at zero', () => {
  const ctx = new RunContext(new Metrics(), QUIET, 0, performance.now() - 1000);
  assert.equal(ctx.deadlineReached(), true);
  assert.equal(ctx.msRemaining(), 0);
  ctx.setDeadline(future());
  assert.equal(ctx.deadlineReached(), false);
  assert.ok(ctx.msRemaining() > 0);
});

test('sleep resolves', async () => {
  assert.equal(await sleep(1), undefined);
});
