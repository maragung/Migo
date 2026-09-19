#!/usr/bin/env node
/**
 * The gate a shell harness puts between a step's exit code and its verdict.
 *
 * `main.ts` answers "did the run exceed a budget?". This answers "was there a run?", which is the
 * question that was missing: a step can exit zero, write nothing, and be recorded as a pass. The
 * harness calls this with the report path and the claim the step makes about itself; a zero exit
 * means the report supports the claim, and anything else prints the reason to stderr and exits 6.
 *
 * It is a separate entry point rather than a flag on `main.ts` because the two read different
 * inputs — one runs a load test, the other reads a file — and because a harness that has to run a
 * load test to check whether the last one happened is a harness nobody will wire up.
 *
 * Usage:
 *   node dist/judge.js --step idle-10k [--min-connected 10000] [--min-hold-ratio 0.9]
 *                      [--max-error-rate 0.05] <report.json>
 */

import { judgeReport, readReport } from './verdict.js';
import type { ReportExpectations } from './verdict.js';

/** The report does not support a pass: the step must not be recorded as one. */
const EXIT_UNSUPPORTED = 6;

const HELP = `usage: judge.js --step <name> [options] <report.json>

options:
  --min-connected <n>     sessions the step requires (default 1)
  --min-hold-ratio <r>    fraction of the target duration that must have been held (default 0.9)
  --max-error-rate <r>    the budget the step ran with (default 0.05)
  -h, --help              this text

exit codes: 0 the report supports the step, 2 bad usage, 6 it does not.
`;

interface Parsed {
  readonly step: string;
  readonly reportPath: string;
  readonly expectations: Omit<ReportExpectations, 'step'>;
}

function parse(
  argv: readonly string[],
): Parsed | { readonly help: true } | { readonly error: string } {
  let step: string | undefined;
  let reportPath: string | undefined;
  let minConnected: number | undefined;
  let minHoldRatio: number | undefined;
  let maxErrorRate: number | undefined;

  const readNumber = (
    flag: string,
    raw: string | undefined,
  ): number | { readonly error: string } => {
    const value = Number(raw);
    return raw !== undefined && Number.isFinite(value)
      ? value
      : { error: `${flag} needs a number` };
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '-h' || arg === '--help') return { help: true };
    if (arg === '--step') {
      step = argv[index + 1];
      index += 1;
      continue;
    }
    if (arg === '--min-connected' || arg === '--min-hold-ratio' || arg === '--max-error-rate') {
      const value = readNumber(arg, argv[index + 1]);
      if (typeof value !== 'number') return value;
      if (arg === '--min-connected') minConnected = value;
      if (arg === '--min-hold-ratio') minHoldRatio = value;
      if (arg === '--max-error-rate') maxErrorRate = value;
      index += 1;
      continue;
    }
    if (arg !== undefined && arg.startsWith('-')) return { error: `unknown option ${arg}` };
    if (reportPath !== undefined) return { error: `unexpected extra argument ${String(arg)}` };
    reportPath = arg;
  }

  if (step === undefined) return { error: '--step is required' };
  if (reportPath === undefined) return { error: 'a report path is required' };
  return {
    step,
    reportPath,
    expectations: {
      ...(minConnected !== undefined ? { minConnected } : {}),
      ...(minHoldRatio !== undefined ? { minHoldRatio } : {}),
      ...(maxErrorRate !== undefined ? { maxErrorRate } : {}),
    },
  };
}

async function main(): Promise<number> {
  const parsed = parse(process.argv.slice(2));
  if ('help' in parsed) {
    process.stdout.write(HELP);
    return 0;
  }
  if ('error' in parsed) {
    process.stderr.write(`judge: ${parsed.error}\n\n${HELP}`);
    return 2;
  }

  const read = await readReport(parsed.reportPath);
  const verdict =
    read.kind === 'missing'
      ? { ok: false, reason: 'no-report', detail: `step '${parsed.step}': ${read.detail}` }
      : judgeReport(read.document, { step: parsed.step, ...parsed.expectations });

  if (!verdict.ok) {
    process.stderr.write(`judge: ${verdict.detail} [${verdict.reason}]\n`);
    return EXIT_UNSUPPORTED;
  }
  process.stdout.write(`  ${verdict.detail}\n`);
  return 0;
}

main()
  .then((code) => {
    process.exitCode = code;
  })
  .catch((error: unknown) => {
    process.stderr.write(
      `judge: fatal: ${error instanceof Error ? error.message : String(error)}\n`,
    );
    process.exitCode = 1;
  });
