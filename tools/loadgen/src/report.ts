/**
 * Turning a finished run into something to read — human text or machine JSON.
 *
 * The report is deliberately blunt about failure. It never rolls errors into a single "success
 * rate"; it breaks them out by class per operation, because "5 RATE_LIMITED" and "5 transport" are
 * different diagnoses. Latency lines carry p50/p90/p99, not an average alone, since the tail is
 * where a real system's trouble hides.
 */

import type { Config } from './config.js';
import type { DigestSnapshot, Metrics } from './stats.js';

export interface RunOutcome {
  readonly config: Config;
  readonly scenarioName: string;
  readonly requestedVus: number;
  readonly connectedCount: number;
  readonly durationMsActual: number;
  readonly interrupted: boolean;
  readonly metrics: Metrics;
}

/** Labels that name a lifecycle phase rather than a steady-state operation with a throughput. */
const PHASE_LABELS = new Set(['connect', 'setup', 'event', 'workload']);

/** Display order; any label not listed sorts after these, alphabetically. */
const LABEL_ORDER = ['connect', 'setup', 'send', 'presence', 'event', 'workload'];

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

export function renderText(outcome: RunOutcome): string {
  const { config, metrics } = outcome;
  const durationSec = outcome.durationMsActual / 1000;
  const lines: string[] = [];

  lines.push(
    `Migo load test — scenario "${outcome.scenarioName}"${outcome.interrupted ? ' (interrupted)' : ''}`,
  );
  lines.push(`  server         ${config.apiUrl}  (${config.gatewayUrl})`);
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
  }

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
  const operations = orderedLabels(metrics).map((label) => {
    const op = metrics.operation(label);
    return {
      label: op.label,
      ok: op.ok,
      errors: op.errors,
      errorsByClass: Object.fromEntries(op.errorsByClass),
      throughputPerSec: PHASE_LABELS.has(label) || durationSec === 0 ? null : op.ok / durationSec,
      latency: op.latency,
    };
  });

  const document = {
    scenario: outcome.scenarioName,
    interrupted: outcome.interrupted,
    server: { api: config.apiUrl, gateway: config.gatewayUrl },
    requestedVus: outcome.requestedVus,
    connectedCount: outcome.connectedCount,
    durationMs: outcome.durationMsActual,
    targetDurationMs: config.durationMs,
    ratePerSec: config.ratePerSec,
    appVersion: config.appVersion,
    errorRate: computeErrorRate(outcome),
    ok: isOk(outcome),
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
    `p50 ${ms(latency.p50)}  p90 ${ms(latency.p90)}  p99 ${ms(latency.p99)}` +
    `  (min ${ms(latency.min)} max ${ms(latency.max)})`
  );
}

function ms(value: number): string {
  return `${value.toFixed(1)}ms`;
}
