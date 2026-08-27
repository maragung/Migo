/**
 * The pool's one job is to bound concurrency: ramping thousands of VUs must measure the server, not
 * the client's file-descriptor limit. So the cap is the headline assertion — instrument the worker
 * to track peak in-flight count and prove it never exceeds the limit, and that it actually fills the
 * slots rather than serializing. The rest pins the contract the scenarios rely on: every item runs
 * once, draining resolves rather than hanging, a worker that catches its own failure leaves the pool
 * healthy, and — by explicit design — an *uncaught* throw rejects the whole pool.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { runPool } from '../pool.js';
import { sleep } from '../run-context.js';

/** Drive `runPool`, tracking peak concurrency and the order items were picked up. */
async function drive(
  itemCount: number,
  limit: number,
  body: (index: number) => Promise<void>,
): Promise<{ maxActive: number; order: number[] }> {
  const items = Array.from({ length: itemCount }, (_v, i) => i);
  let active = 0;
  let maxActive = 0;
  const order: number[] = [];
  await runPool(items, limit, async (item) => {
    order.push(item);
    active += 1;
    maxActive = Math.max(maxActive, active);
    try {
      await body(item);
    } finally {
      active -= 1;
    }
  });
  return { maxActive, order };
}

test('an empty item list resolves without ever calling the worker', async () => {
  let calls = 0;
  await runPool([], 5, () => {
    calls += 1;
    return Promise.resolve();
  });
  assert.equal(calls, 0);
});

test('every item is processed exactly once and the pool resolves', async () => {
  const seen = new Map<number, number>();
  await runPool(
    Array.from({ length: 10 }, (_v, i) => i),
    3,
    (item) => {
      seen.set(item, (seen.get(item) ?? 0) + 1);
      return Promise.resolve();
    },
  );
  assert.equal(seen.size, 10);
  for (let i = 0; i < 10; i += 1) assert.equal(seen.get(i), 1);
});

test('concurrency never exceeds the limit and fills every slot', async () => {
  const { maxActive } = await drive(12, 3, () => sleep(5));
  assert.ok(maxActive <= 3, `peak concurrency ${maxActive} must not exceed the limit`);
  assert.equal(maxActive, 3, 'all three slots should be in use at once');
});

test('the width is clamped to the item count when the limit is larger', async () => {
  const { maxActive } = await drive(4, 100, () => sleep(5));
  assert.equal(maxActive, 4);
});

test('a limit of 1 runs strictly sequentially, in order', async () => {
  const { maxActive, order } = await drive(5, 1, () => sleep(1));
  assert.equal(maxActive, 1);
  assert.deepEqual(order, [0, 1, 2, 3, 4]);
});

test('a worker that catches its own failure leaves the pool healthy and draining', async () => {
  // This is the pattern the scenarios use: per-item failure is caught inside the worker.
  const invoked: number[] = [];
  const succeeded: number[] = [];
  let active = 0;
  let maxActive = 0;
  await runPool(
    Array.from({ length: 6 }, (_v, i) => i),
    2,
    async (item) => {
      invoked.push(item);
      active += 1;
      maxActive = Math.max(maxActive, active);
      try {
        await sleep(1);
        if (item === 2) throw new Error('this item failed');
        succeeded.push(item);
      } catch {
        // Swallowed, exactly as a scenario worker does.
      } finally {
        active -= 1;
      }
    },
  );
  assert.deepEqual(
    invoked.sort((a, b) => a - b),
    [0, 1, 2, 3, 4, 5],
  ); // every item still reached the worker
  assert.deepEqual(
    succeeded.sort((a, b) => a - b),
    [0, 1, 3, 4, 5],
  ); // item 2 failed without stopping the rest
  assert.ok(maxActive <= 2);
});

test('an uncaught throw rejects the whole pool — the documented contract, and it settles', async () => {
  await assert.rejects(
    runPool([1, 2, 3, 4], 2, async (item) => {
      await sleep(1);
      if (item === 2) throw new Error('boom');
    }),
    /boom/,
  );
});
