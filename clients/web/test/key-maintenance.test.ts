/**
 * The inbound key maintenance the provider wires: persist, replenish, persist again on publish.
 *
 * Section 163's client-side triggers added a second inbound fact that mutates this device's key
 * store — an accepted `GROUP_KEY_DISTRIBUTE` — beside the long-standing first one, a first message
 * from a new peer. Both owe the same response, so the provider now routes both listeners through
 * one helper, and what a test pins here is that helper's contract:
 *
 *   1. **Either event schedules a persist and fires a replenish.** A stale persisted snapshot is
 *      the difference between a recoverable session and a lost one after a reload.
 *   2. **A replenish that republished schedules a second persist**, because publishing mutates the
 *      store again.
 *   3. **A failed replenish is swallowed** — best-effort, retried on the next inbound event — and
 *      an already-unsubscribed helper hears nothing.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { wireKeyMaintenance } from '../src/lib/migo/key-maintenance.js';
import type { KeyMaintenanceTarget } from '../src/lib/migo/key-maintenance.js';

/** A target double: records listener registrations and replenish calls, replies on demand. */
class FakeTarget implements KeyMaintenanceTarget {
  readonly messageHandlers: Array<() => void> = [];
  readonly keyExchangeHandlers: Array<() => void> = [];
  /** What each replenish call resolves with, in call order; consumed from the front. */
  readonly replenishResults: Array<boolean | Error> = [];
  replenishCalls = 0;

  readonly messaging = {
    onMessage: (handler: () => void): (() => void) => {
      this.messageHandlers.push(handler);
      return () => {
        const at = this.messageHandlers.indexOf(handler);
        if (at >= 0) {
          this.messageHandlers.splice(at, 1);
        }
      };
    },
    onKeyExchange: (handler: () => void): (() => void) => {
      this.keyExchangeHandlers.push(handler);
      return () => {
        const at = this.keyExchangeHandlers.indexOf(handler);
        if (at >= 0) {
          this.keyExchangeHandlers.splice(at, 1);
        }
      };
    },
  };

  replenishPrekeys(): Promise<boolean> {
    this.replenishCalls += 1;
    const result = this.replenishResults.shift() ?? false;
    return result instanceof Error ? Promise.reject(result) : Promise.resolve(result);
  }
}

/** Lets the helper's fire-and-forget promises run. */
async function flush(times = 6): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
  }
}

test('key maintenance: subscribes both inbound facts and answers each the same way', async () => {
  const target = new FakeTarget();
  target.replenishResults.push(false, false);
  const persists: number[] = [];
  const unsubscribe = wireKeyMaintenance(target, () => persists.push(persists.length));

  assert.equal(target.messageHandlers.length, 1, 'the message listener is subscribed');
  assert.equal(target.keyExchangeHandlers.length, 1, 'the key-exchange listener is subscribed');

  // A first message from a new peer: persist, replenish, and no second persist (nothing published).
  target.messageHandlers[0]?.();
  await flush();
  assert.deepEqual(persists, [0]);
  assert.equal(target.replenishCalls, 1);

  // An accepted GROUP_KEY_DISTRIBUTE distribution: the same response, because it mutates the same
  // store through the same mechanics.
  target.keyExchangeHandlers[0]?.();
  await flush();
  assert.deepEqual(persists, [0, 1]);
  assert.equal(target.replenishCalls, 2);

  unsubscribe();
  assert.equal(target.messageHandlers.length, 0, 'unsubscribe removes the message listener');
  assert.equal(
    target.keyExchangeHandlers.length,
    0,
    'unsubscribe removes the key-exchange listener',
  );
});

test('key maintenance: a replenish that republished persists again', async () => {
  const target = new FakeTarget();
  target.replenishResults.push(true);
  const persists: number[] = [];
  wireKeyMaintenance(target, () => persists.push(persists.length));

  target.keyExchangeHandlers[0]?.();
  await flush();
  // Two persists: one for the inbound fact, one for the republished bundle.
  assert.deepEqual(persists, [0, 1]);
  assert.equal(target.replenishCalls, 1);
});

test('key maintenance: a failed replenish is swallowed and never un-persists', async () => {
  const target = new FakeTarget();
  target.replenishResults.push(new Error('server unreachable'));
  const persists: number[] = [];
  wireKeyMaintenance(target, () => persists.push(persists.length));

  // The persist happened before the replenish ran, so the store snapshot is already saved; the
  // replenish failure must not reject anything the caller could observe.
  target.messageHandlers[0]?.();
  await flush();
  assert.deepEqual(persists, [0]);
  assert.equal(target.replenishCalls, 1);

  // And the next inbound event retries: best-effort, not once-only.
  target.replenishResults.push(false);
  target.messageHandlers[0]?.();
  await flush();
  assert.equal(target.replenishCalls, 2);
});
