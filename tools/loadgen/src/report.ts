/**
 * Turning a finished run into something to read — human text or machine JSON.
 *
 * The report is deliberately blunt about failure. It never rolls errors into a single "success
 * rate"; it breaks them out by class per operation, because "5 RATE_LIMITED" and "5 transport" are
 * different diagnoses. Latency lines carry p50/p90/p95/p99, not an average alone, since the tail is
 * where a real system's trouble hides.
 */

import type { Config } from './config.js';
import { sanitizeUrl } from './redact.js';
import type { DigestSnapshot, Metrics } from './stats.js';
import { byteBudgetVerdict, BYTE_BUDGET_HEADROOM } from './wire-bytes.js';
import type { WireByteSummary } from './wire-bytes.js';

export interface RunOutcome {
  readonly config: Config;
  readonly scenarioName: string;
  readonly requestedVus: number;
  readonly connectedCount: number;
  readonly durationMsActual: number;
  readonly interrupted: boolean;
  readonly metrics: Metrics;
  /** The run's gateway wire bytes, reduced to the per-user per-minute figure §171 asks for. */
  readonly wireBytes: WireByteSummary;
}

/**
 * Labels that name a lifecycle phase or a settle-phase verdict rather than a steady-state
 * operation with a throughput — a "calls per second" computed from verdict samples would be
 * meaningless, and a connect-phase count is not an operation rate.
 *
 * Exported because it is also the line `verdict.ts` draws when it asks whether a run measured the
 * work its scenario names: for every scenario but `connect`, a report whose successes are all in
 * these labels measured nothing but the act of connecting. One definition, because two would drift.
 */
export const PHASE_LABELS = new Set([
  'connect',
  'setup',
  'event',
  'workload',
  'fanout-verdict',
  'outage-verdict',
]);

/** Display order; any label not listed sorts after these, alphabetically. */
const LABEL_ORDER = [
  'connect',
  'setup',
  'send',
  'deliver',
  'fanout-deliver',
  'fanout-verdict',
  'voice-upload',
  'call-invite',
  'call-answer',
  'call-sdp',
  'call-setup',
  'call-ice',
  'call-ice-deliver',
  'call-end',
  'presence',
  'event',
  'workload',
  'outage-verdict',
];

export function computeErrorRate(outcome: RunOutcome): number {
  let ok = 0;
  let errors = 0;
  for (const label of outcome.metrics.labels()) {
    const op = outcome.metrics.operation(label);
    ok += op.ok;
    errors += op.errors;
  }
  const total = ok + errors;
  return total === 0 ? 0 : errors / total;
}

/** Whether the run stayed within its configured error budget (drives the exit code). */
export function isOk(outcome: RunOutcome): boolean {
  return computeErrorRate(outcome) <= outcome.config.maxErrorRate;
}

/**
 * Whether the scenario stayed within its byte budget plus §171's 10 percent headroom (drives the
 * exit code alongside {@link isOk}). A scenario without a budget is never failed here — but every
 * scenario in the registry has one, so an undefined budget means a scenario was added without its
 * budget, which the wire-bytes suite refuses to let happen unnoticed.
 */
export function isWithinByteBudget(outcome: RunOutcome): boolean {
  return !byteBudgetVerdict(outcome.scenarioName, outcome.wireBytes).exceeded;
}

export function renderText(outcome: RunOutcome): string {
  const { config, metrics } = outcome;
  const durationSec = outcome.durationMsActual / 1000;
  const lines: string[] = [];

  lines.push(
    `Migo load test — scenario "${outcome.scenarioName}"${outcome.interrupted ? ' (interrupted)' : ''}`,
  );
  lines.push(`  server         ${sanitizeUrl(config.apiUrl)}  (${sanitizeUrl(config.gatewayUrl)})`);
  lines.push(
    `  virtual users  ${outcome.requestedVus} requested, ${outcome.connectedCount} connected`,
  );
  lines.push(
    `  duration       ${durationSec.toFixed(1)}s (target ${(config.durationMs / 1000).toFixed(1)}s)` +
      `, rate ${config.ratePerSec === 0 ? 'unbounded' : `${config.ratePerSec}/s`} per VU`,
  );
  lines.push('');

  for (const label of orderedLabels(metrics)) {
    const op = metrics.operation(label);
    const parts = [
      `  ${label.padEnd(10)}`,
      `ok ${String(op.ok).padStart(6)}`,
      `err ${String(op.errors).padStart(5)}`,
    ];
    if (op.latency.count > 0) parts.push(latencyText(op.latency));
    if (!PHASE_LABELS.has(label) && durationSec > 0)
      parts.push(`~${(op.ok / durationSec).toFixed(1)}/s`);
    lines.push(parts.join('  '));
    if (op.errorsByClass.length > 0) {
      lines.push(`      errors: ${op.errorsByClass.map(([cls, n]) => `${cls} ${n}`).join(', ')}`);
    }
    // The diagnosis the counts cannot carry: one real message per class, so a
    // wall of identical refusals arrives with the field the server blamed.
    for (const [cls, sample] of op.errorSamples) {
      lines.push(`      e.g. ${cls}: ${sample}`);
    }
  }

  const byteVerdict = byteBudgetVerdict(outcome.scenarioName, outcome.wireBytes);
  lines.push('');
  lines.push(wireBytesText(outcome.wireBytes, byteVerdict));

  const errorRate = computeErrorRate(outcome);
  lines.push('');
  lines.push(
    `Result: ${isOk(outcome) ? 'OK' : 'OVER BUDGET'}  (error rate ${(errorRate * 100).toFixed(2)}%` +
      (config.maxErrorRate < 1 ? `, budget ${(config.maxErrorRate * 100).toFixed(2)}%` : '') +
      ')',
  );
  return lines.join('\n');
}

export function renderJson(outcome: RunOutcome): string {
  const { config, metrics } = outcome;
  const durationSec = outcome.durationMsActual / 1000;
  const wb = outcome.wireBytes;
  const byteVerdict = byteBudgetVerdict(outcome.scenarioName, wb);
  const operations = orderedLabels(metrics).map((label) => {
    const op = metrics.operation(label);
    return {
      label: op.label,
      ok: op.ok,
      errors: op.errors,
      errorsByClass: Object.fromEntries(op.errorsByClass),
      errorSamples: Object.fromEntries(op.errorSamples),
      throughputPerSec: PHASE_LABELS.has(label) || durationSec === 0 ? null : op.ok / durationSec,
      latency: op.latency,
    };
  });

  const document = {
    scenario: outcome.scenarioName,
    interrupted: outcome.interrupted,
    server: { api: sanitizeUrl(config.apiUrl), gateway: sanitizeUrl(config.gatewayUrl) },
    requestedVus: outcome.requestedVus,
    connectedCount: outcome.connectedCount,
    durationMs: outcome.durationMsActual,
    targetDurationMs: config.durationMs,
    ratePerSec: config.ratePerSec,
    appVersion: config.appVersion,
    errorRate: computeErrorRate(outcome),
    ok: isOk(outcome),
    wireBytes: {
      users: wb.users,
      minutes: wb.minutes,
      sentBytes: wb.sentBytes,
      receivedBytes: wb.receivedBytes,
      totalBytes: wb.totalBytes,
      perUserBytes: wb.perUserBytes,
      bytesPerUserPerMinute: wb.bytesPerUserPerMinute,
      budget:
        byteVerdict.budget === undefined || byteVerdict.limitBytesPerUserPerMinute === null
          ? null
          : {
              bytesPerUserPerMinute: byteVerdict.budget.bytesPerUserPerMinute,
              limitBytesPerUserPerMinute: byteVerdict.limitBytesPerUserPerMinute,
              anchor: byteVerdict.budget.anchor,
            },
      withinBudget: !byteVerdict.exceeded,
    },
    operations,
  };
  return JSON.stringify(document, null, 2);
}

function orderedLabels(metrics: Metrics): string[] {
  return metrics.labels().sort((a, b) => {
    const ia = LABEL_ORDER.indexOf(a);
    const ib = LABEL_ORDER.indexOf(b);
    if (ia !== -1 && ib !== -1) return ia - ib;
    if (ia !== -1) return -1;
    if (ib !== -1) return 1;
    return a.localeCompare(b);
  });
}

function latencyText(latency: DigestSnapshot): string {
  return (
    `p50 ${ms(latency.p50)}  p90 ${ms(latency.p90)}  p95 ${ms(latency.p95)}  p99 ${ms(latency.p99)}` +
    `  (min ${ms(latency.min)} max ${ms(latency.max)})`
  );
}

/**
 * The §171 wire-byte line: what the run's gateway sockets carried, the per-user per-minute figure,
 * and the scenario's budget verdict. Rounded — the number is read by humans and compared against a
 * budget, and a byte rate with seven decimals implies a precision no amortized per-minute figure
 * has.
 */
function wireBytesText(wb: WireByteSummary, verdict: ReturnType<typeof byteBudgetVerdict>): string {
  const rate = Math.round(wb.bytesPerUserPerMinute);
  const carried =
    `  wire bytes     ${wb.sentBytes} sent, ${wb.receivedBytes} received (${wb.totalBytes} total)` +
    ` over ${wb.users} user${wb.users === 1 ? '' : 's'} in ${wb.minutes.toFixed(1)} min`;
  if (verdict.budget === undefined || verdict.limitBytesPerUserPerMinute === null) {
    return `${carried}\n                 ${rate} B/user/min  (no byte budget defined for this scenario)`;
  }
  return (
    `${carried}\n` +
    `                 ${rate} B/user/min` +
    `  (budget ${verdict.budget.bytesPerUserPerMinute} B/user/min` +
    `, limit +${Math.round(BYTE_BUDGET_HEADROOM * 100)}% ${Math.round(verdict.limitBytesPerUserPerMinute)})` +
    ` — ${verdict.exceeded ? 'OVER BYTE BUDGET' : 'WITHIN'}`
  );
}

function ms(value: number): string {
  return `${value.toFixed(1)}ms`;
}
