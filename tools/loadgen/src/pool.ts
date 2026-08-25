/**
 * A bounded-concurrency worker pool.
 *
 * Ramping thousands of VUs must not open thousands of sockets at once — that would measure the
 * client host's file-descriptor limit, not the server. `runPool` keeps at most `limit` invocations
 * of `worker` in flight, feeding items in order as slots free up, and resolves once all are done. A
 * worker that throws rejects the whole pool, so callers that must tolerate per-item failure (the
 * scenarios do) catch inside the worker.
 */
export async function runPool<T>(
  items: readonly T[],
  limit: number,
  worker: (item: T, index: number) => Promise<void>,
): Promise<void> {
  if (items.length === 0) return;
  const width = Math.max(1, Math.min(limit, items.length));
  let next = 0;

  const run = async (): Promise<void> => {
    while (next < items.length) {
      const index = next;
      next += 1;
      const item = items[index];
      if (item !== undefined) await worker(item, index);
    }
  };

  await Promise.all(Array.from({ length: width }, () => run()));
}
