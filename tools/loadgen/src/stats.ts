/**
 * Latency and outcome accounting for a load run.
 *
 * Latencies are summarised as a digest: exact count, sum, min and max, plus percentiles drawn from
 * a bounded reservoir sample. The reservoir caps memory on long runs — a million sends would
 * otherwise pin a million doubles in the heap — while keeping percentile estimates faithful, since
 * Vitter's Algorithm R gives every observation an equal chance of being retained. Outcomes are
 * tallied per operation label and split into successes and errors-by-class, so a report can say not
 * just how many operations failed but how: a wall of `remote:RATE_LIMITED` means the server pushed
 * back, whereas `transport` means the socket died, and the two call for opposite reactions.
 */

import { RemoteError, TimeoutError, TransportError, SdkError } from '@migo/sdk';

/** Cap on retained latency samples per label. Percentiles are estimated from this reservoir. */
const RESERVOIR_CAP = 100_000;

export interface DigestSnapshot {
  readonly count: number;
  readonly min: number;
  readonly max: number;
  readonly mean: number;
  readonly p50: number;
  readonly p90: number;
  readonly p99: number;
}

export interface OperationSnapshot {
  readonly label: string;
  readonly ok: number;
  readonly errors: number;
  /** Error counts by class, most frequent first. */
  readonly errorsByClass: ReadonlyArray<readonly [string, number]>;
  readonly latency: DigestSnapshot;
}

/** Records latency samples for one operation, computing percentiles from a bounded reservoir. */
export class LatencyDigest {
  #count = 0;
  #sum = 0;
  #min = Number.POSITIVE_INFINITY;
  #max = 0;
  readonly #reservoir: number[] = [];

  record(ms: number): void {
    this.#count += 1;
    this.#sum += ms;
    if (ms < this.#min) this.#min = ms;
    if (ms > this.#max) this.#max = ms;

    if (this.#reservoir.length < RESERVOIR_CAP) {
      this.#reservoir.push(ms);
      return;
    }
    // Algorithm R: the nth sample (n > CAP) replaces a uniformly chosen slot with probability
    // CAP/n, which keeps the reservoir a uniform sample of everything seen so far.
    const j = Math.floor(Math.random() * this.#count);
    if (j < RESERVOIR_CAP) this.#reservoir[j] = ms;
  }

  snapshot(): DigestSnapshot {
    if (this.#count === 0) {
      return { count: 0, min: 0, max: 0, mean: 0, p50: 0, p90: 0, p99: 0 };
    }
    const sorted = [...this.#reservoir].sort((a, b) => a - b);
    return {
      count: this.#count,
      min: this.#min,
      max: this.#max,
      mean: this.#sum / this.#count,
      p50: percentile(sorted, 0.5),
      p90: percentile(sorted, 0.9),
      p99: percentile(sorted, 0.99),
    };
  }
}

/** Nearest-rank percentile over an ascending array. */
function percentile(sorted: readonly number[], q: number): number {
  if (sorted.length === 0) return 0;
  const rank = Math.ceil(q * sorted.length);
  const index = Math.min(sorted.length - 1, Math.max(0, rank - 1));
  return sorted[index] ?? 0;
}

/** Aggregates latency digests and success/error tallies across every operation label in a run. */
export class Metrics {
  readonly #latency = new Map<string, LatencyDigest>();
  readonly #ok = new Map<string, number>();
  readonly #errors = new Map<string, Map<string, number>>();

  /** The latency digest for `label`, created on first use. */
  latency(label: string): LatencyDigest {
    let digest = this.#latency.get(label);
    if (digest === undefined) {
      digest = new LatencyDigest();
      this.#latency.set(label, digest);
    }
    return digest;
  }

  recordOk(label: string): void {
    this.#ok.set(label, (this.#ok.get(label) ?? 0) + 1);
  }

  recordError(label: string, errorClass: string): void {
    let byClass = this.#errors.get(label);
    if (byClass === undefined) {
      byClass = new Map();
      this.#errors.set(label, byClass);
    }
    byClass.set(errorClass, (byClass.get(errorClass) ?? 0) + 1);
  }

  /** Every label that saw a latency sample, a success, or an error. */
  labels(): string[] {
    return [...new Set([...this.#latency.keys(), ...this.#ok.keys(), ...this.#errors.keys()])];
  }

  operation(label: string): OperationSnapshot {
    const byClass = this.#errors.get(label);
    const errorsByClass = byClass ? [...byClass.entries()].sort((a, b) => b[1] - a[1]) : [];
    const errors = errorsByClass.reduce((sum, [, count]) => sum + count, 0);
    return {
      label,
      ok: this.#ok.get(label) ?? 0,
      errors,
      errorsByClass,
      latency: this.latency(label).snapshot(),
    };
  }
}

/**
 * Reduces any thrown value to a stable, low-cardinality class label for tallying.
 *
 * The ordering matters: {@link RemoteError} and the transport errors are checked before their common
 * {@link SdkError} base so they keep their specific class. A server refusal becomes `remote:<SYMBOL>`
 * (e.g. `remote:RATE_LIMITED`) — the symbol, never the human message, which section 161 forbids the
 * server from making meaningful.
 */
export function classifyError(error: unknown): string {
  if (error instanceof RemoteError) return `remote:${error.symbol}`;
  if (error instanceof TimeoutError) return 'timeout';
  if (error instanceof TransportError) return 'transport';
  if (error instanceof SdkError) return 'sdk';
  if (error instanceof Error) return `local:${error.name}`;
  return 'unknown';
}
