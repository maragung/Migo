/**
 * The shared clock and instrumentation a running scenario sees.
 *
 * A {@link RunContext} owns the run deadline and the interrupt flag, and it wraps the two things
 * every workload does over and over: time an operation ({@link RunContext.measure}) and pace a loop
 * to a target rate until the deadline ({@link RunContext.paceLoop}). Keeping both here means the
 * scenarios stay declarative — "send, measured, paced" — and the tricky parts (never throw out of a
 * measured op, always wake promptly on Ctrl-C, honour a server's back-off) live in exactly one place.
 */

import { RemoteError } from '@migo/sdk';

import type { Logger } from './logger.js';
import { classifyError } from './stats.js';
import type { Metrics } from './stats.js';

/** Resolves after `ms` milliseconds. */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/** Longest single sleep while waiting, so an interrupt or the deadline is noticed within this bound. */
const POLL_MS = 200;

export class RunContext {
  readonly metrics: Metrics;
  readonly log: Logger;
  /** Per-VU target operations per second; 0 means run closed-loop, as fast as acks return. */
  readonly ratePerSec: number;

  #deadline: number;
  #interrupted = false;

  constructor(metrics: Metrics, log: Logger, ratePerSec: number, deadline: number) {
    this.metrics = metrics;
    this.log = log;
    this.ratePerSec = ratePerSec;
    this.#deadline = deadline;
  }

  /** Move the deadline (used to switch from the open-ended connect phase to the timed steady state). */
  setDeadline(deadline: number): void {
    this.#deadline = deadline;
  }

  /** Request an early, graceful stop (Ctrl-C / SIGTERM). Loops exit at their next check. */
  interrupt(): void {
    this.#interrupted = true;
  }

  get interrupted(): boolean {
    return this.#interrupted;
  }

  deadlineReached(): boolean {
    return this.#interrupted || performance.now() >= this.#deadline;
  }

  msRemaining(): number {
    return Math.max(0, this.#deadline - performance.now());
  }

  /**
   * Await `operation`, recording its latency on success and its error class on failure. Never throws,
   * so a single failed op cannot tear down the VU's loop. When the server asks for back-off (a
   * {@link RemoteError} carrying `retryAfterMs`), waits it out — a load generator should apply
   * pressure, not hammer a limiter that has already said "slow down".
   */
  async measure(label: string, operation: () => Promise<unknown>): Promise<void> {
    const started = performance.now();
    try {
      await operation();
      this.metrics.latency(label).record(performance.now() - started);
      this.metrics.recordOk(label);
    } catch (error) {
      this.metrics.recordError(label, classifyError(error));
      if (error instanceof RemoteError && error.retryAfterMs !== undefined) {
        await this.#sleepBounded(error.retryAfterMs);
      }
    }
  }

  /** Run `body` in a paced loop until the deadline, honouring {@link ratePerSec}. */
  async paceLoop(body: () => Promise<void>): Promise<void> {
    const started = performance.now();
    let iterations = 0;
    while (!this.deadlineReached()) {
      await body();
      iterations += 1;
      if (this.ratePerSec > 0) {
        // Schedule against a fixed origin rather than "now + interval" so the loop does not drift
        // slower each iteration by the time each op itself took.
        const nextAt = started + (iterations * 1000) / this.ratePerSec;
        await this.#sleepBounded(nextAt - performance.now());
      }
    }
  }

  /** Sleep up to `ms`, waking early if the run is interrupted or the deadline passes. */
  async #sleepBounded(ms: number): Promise<void> {
    if (ms <= 0) return;
    const until = performance.now() + ms;
    while (!this.deadlineReached()) {
      const remaining = until - performance.now();
      if (remaining <= 0) return;
      await sleep(Math.min(remaining, POLL_MS));
    }
  }
}
