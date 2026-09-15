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

import { redact } from './redact.js';

/** Cap on retained latency samples per label. Percentiles are estimated from this reservoir. */
const RESERVOIR_CAP = 100_000;

/** Cap on a retained error sample's length, so one runaway message cannot bloat the report. */
const SAMPLE_CAP = 200;

export interface DigestSnapshot {
  readonly count: number;
  readonly min: number;
  readonly max: number;
  readonly mean: number;
  readonly p50: number;
  readonly p90: number;
  readonly p95: number;
  readonly p99: number;
}

export interface OperationSnapshot {
  readonly label: string;
  readonly ok: number;
  readonly errors: number;
  /** Error counts by class, most frequent first. */
  readonly errorsByClass: ReadonlyArray<readonly [string, number]>;
  /**
   * One retained error sample per class — the first the run saw — as `describeError` shaped it.
   * The class label says how many failed and in what way; the sample says why, which for a server
   * refusal is the field the server blamed. Without it a wall of `remote:VALIDATION_FAILED` is a
   * count with no diagnosis, and the only copy of the answer sits in a log nobody reads.
   */
  readonly errorSamples: ReadonlyArray<readonly [string, string]>;
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
      return { count: 0, min: 0, max: 0, mean: 0, p50: 0, p90: 0, p95: 0, p99: 0 };
    }
    const sorted = [...this.#reservoir].sort((a, b) => a - b);
    return {
      count: this.#count,
      min: this.#min,
      max: this.#max,
      mean: this.#sum / this.#count,
      p50: percentile(sorted, 0.5),
      p90: percentile(sorted, 0.9),
      // Section 172's load metrics name p50, p95, and p99; p90 stays because the
      // messaging budgets were written against it and a regression gate should not
      // silently change the percentile it reads.
      p95: percentile(sorted, 0.95),
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
  readonly #samples = new Map<string, Map<string, string>>();

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

  recordError(label: string, errorClass: string, sample?: string): void {
    let byClass = this.#errors.get(label);
    if (byClass === undefined) {
      byClass = new Map();
      this.#errors.set(label, byClass);
    }
    byClass.set(errorClass, (byClass.get(errorClass) ?? 0) + 1);
    // The first sample per class is the one kept: one is enough to diagnose, and
    // the first is the one that most plausibly names the systematic cause.
    if (sample !== undefined && sample !== '') {
      let bySample = this.#samples.get(label);
      if (bySample === undefined) {
        bySample = new Map();
        this.#samples.set(label, bySample);
      }
      if (!bySample.has(errorClass)) bySample.set(errorClass, sample);
    }
  }

  /** Every label that saw a latency sample, a success, or an error. */
  labels(): string[] {
    return [...new Set([...this.#latency.keys(), ...this.#ok.keys(), ...this.#errors.keys()])];
  }

  operation(label: string): OperationSnapshot {
    const byClass = this.#errors.get(label);
    const errorsByClass = byClass ? [...byClass.entries()].sort((a, b) => b[1] - a[1]) : [];
    const errors = errorsByClass.reduce((sum, [, count]) => sum + count, 0);
    const bySample = this.#samples.get(label);
    const errorSamples = bySample ? [...bySample.entries()] : [];
    return {
      label,
      ok: this.#ok.get(label) ?? 0,
      errors,
      errorsByClass,
      errorSamples,
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

/**
 * A short, redacted, single-line rendering of an error for the report's per-class sample.
 *
 * For a server refusal the offending field leads — `fault::validation(field, …)` names the field,
 * and the field name is the whole diagnosis for a wall of identical `remote:VALIDATION_FAILED` —
 * with the server's public message (already symbol-prefixed by the SDK) after it. Everything else
 * contributes its message when it has one. Capped and scrubbed, because the report gets pasted
 * where credentials must not travel.
 */
export function describeError(error: unknown): string | undefined {
  let detail: string | undefined;
  if (error instanceof RemoteError) {
    detail = error.field === undefined ? error.message : `${error.field}: ${error.message}`;
  } else if (error instanceof Error) {
    detail = error.message;
  }
  if (detail === undefined || detail === '') return undefined;
  const capped = detail.length > SAMPLE_CAP ? `${detail.slice(0, SAMPLE_CAP)}…` : detail;
  return redact(capped);
}
