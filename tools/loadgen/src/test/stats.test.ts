/**
 * The accounting a load run stands on.
 *
 * Percentiles are the highest-value target here: an off-by-one in the nearest-rank index is
 * invisible in production yet silently invalidates every report the tool prints. So the expected
 * values below are computed by hand from the documented rule — `rank = ceil(q * n)`, value at
 * `rank - 1`, clamped — at n=1, n=2, an even n and an odd n, and a ten-element sample whose
 * percentiles are all distinct so an index that is one off lands on the wrong number. The empty
 * digest is pinned separately: it must never divide by zero or surface NaN/Infinity. `classifyError`
 * is checked against the SDK's real error types, including the ordering trap that a `RemoteError`
 * (which extends `SdkError`) keeps its specific class rather than collapsing to `sdk`.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { RemoteError, SdkError, TimeoutError, TransportError } from '@migo/sdk';

import { classifyError, LatencyDigest, Metrics } from '../stats.js';

function digestOf(samples: readonly number[]): ReturnType<LatencyDigest['snapshot']> {
  const digest = new LatencyDigest();
  for (const sample of samples) digest.record(sample);
  return digest.snapshot();
}

test('empty digest is all zeros and never NaN or Infinity', () => {
  const snap = new LatencyDigest().snapshot();
  for (const value of [snap.count, snap.min, snap.max, snap.mean, snap.p50, snap.p90, snap.p99]) {
    assert.equal(value, 0);
    assert.ok(Number.isFinite(value), 'every field must be finite for an empty sample');
  }
});

test('n=1: every percentile is the single sample', () => {
  const snap = digestOf([42]);
  assert.equal(snap.count, 1);
  assert.equal(snap.min, 42);
  assert.equal(snap.max, 42);
  assert.equal(snap.mean, 42);
  assert.equal(snap.p50, 42);
  assert.equal(snap.p90, 42);
  assert.equal(snap.p99, 42);
});

test('n=2: nearest-rank splits the pair exactly', () => {
  // rank(p50) = ceil(0.5*2) = 1 -> index 0 -> 10; rank(p90) = ceil(1.8) = 2 -> index 1 -> 20.
  const snap = digestOf([10, 20]);
  assert.equal(snap.count, 2);
  assert.equal(snap.mean, 15);
  assert.equal(snap.min, 10);
  assert.equal(snap.max, 20);
  assert.equal(snap.p50, 10);
  assert.equal(snap.p90, 20);
  assert.equal(snap.p99, 20);
});

test('even n=4: hand-computed nearest-rank indices', () => {
  // p50: ceil(0.5*4)=2 -> idx1 -> 20; p90: ceil(0.9*4)=ceil(3.6)=4 -> idx3 -> 40.
  const snap = digestOf([10, 20, 30, 40]);
  assert.equal(snap.mean, 25);
  assert.equal(snap.p50, 20);
  assert.equal(snap.p90, 40);
  assert.equal(snap.p99, 40);
});

test('odd n=5: hand-computed nearest-rank indices', () => {
  // p50: ceil(0.5*5)=ceil(2.5)=3 -> idx2 -> 3; p90: ceil(4.5)=5 -> idx4 -> 5.
  const snap = digestOf([1, 2, 3, 4, 5]);
  assert.equal(snap.mean, 3);
  assert.equal(snap.min, 1);
  assert.equal(snap.max, 5);
  assert.equal(snap.p50, 3);
  assert.equal(snap.p90, 5);
  assert.equal(snap.p99, 5);
});

test('n=10 with distinct percentiles catches an off-by-one, and sorts first', () => {
  // Recorded out of order to prove the digest sorts before ranking.
  // p50: ceil(0.5*10)=5 -> idx4 -> 50; p90: ceil(9)=9 -> idx8 -> 90; p99: ceil(9.9)=10 -> idx9 -> 100.
  const snap = digestOf([50, 10, 90, 30, 70, 20, 100, 40, 80, 60]);
  assert.equal(snap.count, 10);
  assert.equal(snap.min, 10);
  assert.equal(snap.max, 100);
  assert.equal(snap.mean, 55);
  assert.equal(snap.p50, 50);
  assert.equal(snap.p90, 90);
  assert.equal(snap.p99, 100);
});

test('count, sum-derived mean, min and max are exact regardless of ordering', () => {
  const digest = new LatencyDigest();
  for (let i = 1; i <= 250; i += 1) digest.record(i);
  const snap = digest.snapshot();
  assert.equal(snap.count, 250);
  assert.equal(snap.min, 1);
  assert.equal(snap.max, 250);
  assert.equal(snap.mean, 125.5); // (1 + 250) / 2
});

test('classifyError maps a server refusal to remote:<SYMBOL>, from the symbol not the message', () => {
  assert.equal(
    classifyError(new RemoteError(429, 'RATE_LIMITED', '', 1000)),
    'remote:RATE_LIMITED',
  );
  assert.equal(
    classifyError(new RemoteError(500, 'INTERNAL_ERROR', 'a human hint that must not be used')),
    'remote:INTERNAL_ERROR',
  );
});

test('classifyError keeps a RemoteError specific even though it extends SdkError', () => {
  const err = new RemoteError(429, 'RATE_LIMITED', '');
  assert.ok(err instanceof SdkError, 'guards the ordering trap this test exists for');
  assert.equal(classifyError(err), 'remote:RATE_LIMITED');
});

test('classifyError maps each transport-level SDK error to its own class', () => {
  assert.equal(classifyError(new TimeoutError('deadline')), 'timeout');
  assert.equal(classifyError(new TransportError('socket closed')), 'transport');
  assert.equal(classifyError(new SdkError('generic')), 'sdk');
});

test('classifyError labels a plain Error by its name and everything else unknown', () => {
  assert.equal(classifyError(new Error('boom')), 'local:Error');
  assert.equal(classifyError(new TypeError('bad')), 'local:TypeError');
  assert.equal(classifyError('a string'), 'unknown');
  assert.equal(classifyError(undefined), 'unknown');
  assert.equal(classifyError(null), 'unknown');
  assert.equal(classifyError(42), 'unknown');
  assert.equal(classifyError({ not: 'an error' }), 'unknown');
});

test('Metrics tallies successes and errors per label', () => {
  const metrics = new Metrics();
  metrics.recordOk('send');
  metrics.recordOk('send');
  metrics.recordOk('send');
  const op = metrics.operation('send');
  assert.equal(op.ok, 3);
  assert.equal(op.errors, 0);
  assert.deepEqual(op.errorsByClass, []);
});

test('Metrics breaks errors out by class, most frequent first', () => {
  const metrics = new Metrics();
  metrics.recordError('send', 'transport');
  metrics.recordError('send', 'transport');
  metrics.recordError('send', 'remote:RATE_LIMITED');
  const op = metrics.operation('send');
  assert.equal(op.errors, 3);
  assert.deepEqual(op.errorsByClass, [
    ['transport', 2],
    ['remote:RATE_LIMITED', 1],
  ]);
});

test('Metrics.latency memoizes one digest per label', () => {
  const metrics = new Metrics();
  const first = metrics.latency('send');
  assert.equal(metrics.latency('send'), first);
  first.record(12);
  assert.equal(metrics.operation('send').latency.count, 1);
});

test('Metrics.labels is the union of every label seen anywhere', () => {
  const metrics = new Metrics();
  metrics.latency('a').record(1);
  metrics.recordOk('b');
  metrics.recordError('c', 'transport');
  const labels = metrics.labels().sort();
  assert.deepEqual(labels, ['a', 'b', 'c']);
});

test('Metrics.operation for an unseen label is empty, not undefined', () => {
  const op = new Metrics().operation('never-touched');
  assert.equal(op.ok, 0);
  assert.equal(op.errors, 0);
  assert.equal(op.latency.count, 0);
  assert.deepEqual(op.errorsByClass, []);
});
