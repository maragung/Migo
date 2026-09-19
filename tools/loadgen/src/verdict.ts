/**
 * Whether a report is a verdict, or just a file.
 *
 * A load step's exit code answers "did loadgen finish without exceeding a budget?" — and that is
 * not the same question as "did the step do the thing it is named for?". A run that opened no
 * session, held nothing open, and measured nothing has not exceeded any budget: it has an error
 * rate of zero over a denominator of zero, which the exit code reports as success. The report says
 * so plainly, and nobody reads the report.
 *
 * So this module reads it. {@link judgeReport} takes the JSON loadgen wrote and the claim the step
 * makes about itself — how many sessions it opened, how long it held them, how much error it
 * tolerates — and answers with the verdict the exit code cannot give. It is deliberately separate
 * from `report.ts`: that one renders what happened, this one decides whether what happened was the
 * experiment, and a harness that mixes the two ends up trusting the thing it is supposed to check.
 *
 * The case that matters most is the one that produced no report at all. Loadgen's report is written
 * once, at the end, from a run that reached its end; a run whose event loop drained mid-flight
 * writes nothing, exits zero, and leaves the harness an empty file. {@link readReport} exists for
 * exactly that file, and says "missing" rather than "no error found".
 */

import { readFile } from 'node:fs/promises';

import { PHASE_LABELS } from './report.js';

/** What a step claims about itself, for {@link judgeReport} to hold the report to. */
export interface ReportExpectations {
  /** The step's name, for the message. */
  readonly step: string;
  /** Sessions that must have opened. Default 1: a step that opened none is not that step. */
  readonly minConnected?: number;
  /**
   * The fraction of its target duration the run must actually have held. Default 0.9.
   *
   * A run that spends its whole window in the connect phase and returns early has a duration far
   * below its target while still exiting zero, because nothing in it went wrong — it simply did not
   * happen. The hold is the honest floor under that.
   */
  readonly minHoldRatio?: number;
  /** The error rate the step tolerates, matching loadgen's `--max-error-rate`. Default 0.05. */
  readonly maxErrorRate?: number;
}

/** The verdict on one report. `reason` is a stable slug for scripts; `detail` is the sentence. */
export interface ReportVerdict {
  readonly ok: boolean;
  readonly reason: string;
  readonly detail: string;
}

/** A report that could be read, or the reason there was nothing to read. */
export type ReportRead =
  | { readonly kind: 'report'; readonly document: Record<string, unknown> }
  | { readonly kind: 'missing'; readonly detail: string };

/**
 * Reads a report file, treating an empty one as the absence it is.
 *
 * An empty file is not a malformed report — it is the fingerprint of a run that died without
 * writing, and the two want different sentences: one says "this file is not JSON", the other says
 * "no run wrote this". The caller gets the difference.
 */
export async function readReport(path: string): Promise<ReportRead> {
  let text: string;
  try {
    text = await readFile(path, 'utf8');
  } catch (cause) {
    return {
      kind: 'missing',
      detail: `no report at ${path}: ${cause instanceof Error ? cause.message : String(cause)}`,
    };
  }
  if (text.trim() === '') {
    return {
      kind: 'missing',
      detail:
        `the report at ${path} is empty — the run it should describe wrote nothing, which is what a ` +
        'run that died with the event loop empty leaves behind',
    };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (cause) {
    return {
      kind: 'missing',
      detail: `the report at ${path} is not JSON: ${cause instanceof Error ? cause.message : String(cause)}`,
    };
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return { kind: 'missing', detail: `the report at ${path} is not a JSON object` };
  }
  return { kind: 'report', document: parsed as Record<string, unknown> };
}

function numberAt(document: Record<string, unknown>, key: string): number | undefined {
  const value = document[key];
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

/**
 * The verdict on a report the step has already read.
 *
 * The rules run in order of how fundamental the claim is, so the message names the first thing that
 * is wrong rather than all of them: a report with no sessions in it also has no operations, and
 * "no session opened" is the diagnosis while "no operation was measured" is its consequence.
 */
export function judgeReport(
  document: Record<string, unknown>,
  expectations: ReportExpectations,
): ReportVerdict {
  const { step } = expectations;
  const minConnected = expectations.minConnected ?? 1;
  const minHoldRatio = expectations.minHoldRatio ?? 0.9;
  const maxErrorRate = expectations.maxErrorRate ?? 0.05;

  const rawScenario = document['scenario'];
  const scenario = typeof rawScenario === 'string' ? rawScenario : 'unknown';
  const connected = numberAt(document, 'connectedCount');
  const durationMs = numberAt(document, 'durationMs');
  const targetDurationMs = numberAt(document, 'targetDurationMs');
  if (connected === undefined || durationMs === undefined || targetDurationMs === undefined) {
    return {
      ok: false,
      reason: 'not-a-report',
      detail:
        `step '${step}': the report does not carry connectedCount, durationMs and targetDurationMs, ` +
        'so it cannot be judged — loadgen may have changed its report shape without this gate noticing',
    };
  }

  if (document['interrupted'] === true) {
    return {
      ok: false,
      reason: 'interrupted',
      detail:
        `step '${step}': the run was interrupted before its deadline, so the window it reports is ` +
        'not the window the step asked for',
    };
  }

  if (connected < minConnected) {
    return {
      ok: false,
      reason: 'no-session-opened',
      detail:
        `step '${step}': ${connected} session(s) connected where the step requires ${minConnected} — ` +
        `the scenario "${scenario}" never ran, so nothing about the server was measured`,
    };
  }

  const held = targetDurationMs > 0 ? durationMs / targetDurationMs : 1;
  if (held < minHoldRatio) {
    return {
      ok: false,
      reason: 'ended-early',
      detail:
        `step '${step}': the run held ${(durationMs / 1000).toFixed(1)}s of its ` +
        `${(targetDurationMs / 1000).toFixed(1)}s window (${(held * 100).toFixed(0)}%, floor ` +
        `${(minHoldRatio * 100).toFixed(0)}%) — the load it claims to have applied was never applied`,
    };
  }

  const operations = document['operations'];
  if (!Array.isArray(operations)) {
    return {
      ok: false,
      reason: 'not-a-report',
      detail: `step '${step}': the report carries no operations array, so there is nothing it measured`,
    };
  }
  let ok = 0;
  let phaseOk = 0;
  let measuredOk = 0;
  for (const entry of operations as unknown[]) {
    if (typeof entry !== 'object' || entry === null) continue;
    const record = entry as Record<string, unknown>;
    const count = record['ok'];
    if (typeof count !== 'number' || !Number.isFinite(count)) continue;
    ok += count;
    // A lifecycle label counts that the run got as far as it did, never that the scenario ran: the
    // connect phase succeeds in every run that opens a socket, including one that opens a socket and
    // then does nothing else for its whole window. Only the scenario's own operations are a
    // measurement of the scenario — except for the `connect` scenario, whose work is connecting, and
    // which the hold ratio above and `minConnected` already hold to its real shape.
    const label = record['label'];
    if (typeof label === 'string' && PHASE_LABELS.has(label)) {
      phaseOk += count;
    } else {
      measuredOk += count;
    }
  }
  const measured = scenario === 'connect' ? phaseOk : measuredOk;
  if (measured === 0) {
    return {
      ok: false,
      reason: 'no-measurement',
      detail:
        `step '${step}': the scenario "${scenario}" counted no successful operation of its own — ` +
        `${phaseOk} success(es) in lifecycle phases and ${measuredOk} at the work it is named for, ` +
        'which is a run that connected and then did nothing',
    };
  }

  const errorRate = numberAt(document, 'errorRate') ?? 1;
  if (errorRate > maxErrorRate) {
    return {
      ok: false,
      reason: 'over-budget',
      detail:
        `step '${step}': error rate ${(errorRate * 100).toFixed(2)}% is above the ` +
        `${(maxErrorRate * 100).toFixed(2)}% budget the step ran with`,
    };
  }

  return {
    ok: true,
    reason: 'measured',
    detail:
      `step '${step}': ${connected} session(s), ${(durationMs / 1000).toFixed(1)}s held, ` +
      `${ok} operation(s) measured, error rate ${(errorRate * 100).toFixed(2)}%`,
  };
}
