/** Small, dependency-free timing helpers for the UI. */

/**
 * A trailing-edge debounce: many calls inside the quiet window collapse into one.
 *
 * The friend graph is the reason this exists. A friend event says the graph moved, not how, so
 * every surface that shows it re-reads the whole graph rather than patching local state — and
 * the wire's own coalescing means one acceptance can arrive as several events (the request's
 * removal and the friendship's arrival, each echoed per device). One read per burst of quiet is
 * the honest rate: the last call's arguments win, because the last event is the freshest word on
 * what moved, and a read fired mid-burst would fetch a graph the next event was about to
 * supersede anyway.
 *
 * `cancel` drops a pending call without firing it — the cleanup an effect owes when the surface
 * it refreshed unmounts, so an unmounted component never pays for a read it can no longer show.
 */
export interface Debounced<A extends unknown[]> {
  (...args: A): void;
  /** Drops the pending call, if one is waiting, without ever firing it. */
  cancel(): void;
}

/**
 * Wraps `fn` so that a burst of calls within `waitMs` of each other costs one invocation, made
 * with the last call's arguments once the burst has gone quiet.
 */
export function debounce<A extends unknown[]>(
  fn: (...args: A) => void,
  waitMs: number,
): Debounced<A> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const wrapped: Debounced<A> = (...args: A): void => {
    if (timer !== null) {
      clearTimeout(timer);
    }
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, waitMs);
  };
  wrapped.cancel = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };
  return wrapped;
}
